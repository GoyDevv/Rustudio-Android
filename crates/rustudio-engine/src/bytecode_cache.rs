//! Compiled scripts kept by source: a Play session compiles every
//! ModuleScript the server and the client each require, and a place with
//! data modules of tens of megabytes (a large ported place: 150 MB of
//! Luau, one module 47 MB) spent seconds compiling the same text twice per
//! Play and again on every run. Bytecode of sources over MIN_MEMORY is kept
//! for the process; over MIN_DISK (on Android: 1 KB) also in
//! <cache>/bytecode/, keyed by the source and the compiler (settings and
//! its output for a probe). RUSTUDIO_NO_BYTECODE_CACHE=1 turns it off.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// (Android: 512 B, desktop: 1 KB — a Play compiles most ModuleScripts twice,
/// once a side; below this the hash costs what the compiler does.)
const MIN_MEMORY: usize = if cfg!(target_os = "android") { 512 } else { 1024 };
/// On a phone every launch is a cold process and the compiler is slower
/// (a fighting game's 685 scripts, 7 MB of Luau, compiled again each
/// launch otherwise): nearly everything the memory cache keeps goes to
/// disk too (a few MB, one file a script, in the app's cache directory).
const MIN_DISK: usize = if cfg!(target_os = "android") { 1024 } else { 256 * 1024 };
/// Bytecode held in memory at most (later sources compile as before).
const MEMORY_CAP: usize = if cfg!(target_os = "android") { 32 * 1024 * 1024 } else { 768 * 1024 * 1024 };

/// Monotonic suffix for atomic `.tmp` writes so concurrent threads in the
/// same process never race on the same temp file.
static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Generation counter for background prewarm jobs: opening a new place bumps
/// this so stale prewarm threads exit promptly.
static PREWARM_GEN: AtomicU64 = AtomicU64::new(0);

fn opt_level() -> u8 {
    static LEVEL: OnceLock<u8> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        if std::env::var_os("RUSTUDIO_LUAU_O1").is_some_and(|v| v == "1") {
            1
        } else {
            2
        }
    })
}

/// The compiler every script runs through: Studio's options (runtime.rs
/// Runtime::new explains each), with Luau -O2 by default (`RUSTUDIO_LUAU_O1=1`
/// falls back to -O1).
pub fn compiler() -> mlua::chunk::Compiler {
    mlua::chunk::Compiler::new()
        .set_optimization_level(opt_level())
        .set_debug_level(1)
        .set_vector_ctor("Vector3.new")
        .set_vector_type("Vector3")
        // (Roblox's list. Imports now resolve when a script loads (vendor
        // lvmload.cpp resolveImportSafe), so chains through these, which
        // scripts change, must stay run-time lookups: `_G.Config` set by
        // another script after this one loaded.)
        .set_mutable_globals(["Game", "Workspace", "_G", "game", "plugin", "script", "shared", "workspace"])
}

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_NO_BYTECODE_CACHE").is_none_or(|v| v == "0"))
}

/// What the bytecode depends on besides the source: the compiler settings
/// and the compiler itself, fingerprinted by its output for a probe chunk
/// (a Luau update that changes code generation or the bytecode version
/// changes it; rebuilding rustudio alone does not).
fn salt() -> &'static [u8] {
    static SALT: OnceLock<Vec<u8>> = OnceLock::new();
    SALT.get_or_init(|| {
        const PROBE: &str = r##"local t = {1, 2.5, "s", true, nil, x = {y = vector.create(1, 2, 3)}, [10] = -0.0}
local function f(a, ...) local s = `{a}:{select("#", ...)}` for i, v in ipairs(t) do s ..= tostring(v) end return s, #t end
local v = Vector3.new(1, 2, 3) local g = game:GetService("Workspace") workspace.Name = script.Name
return f(1, 2, 3), math.floor(v.X // 2) % 3, (if t[1] then 1 else 2) + bit32.band(7, 3), string.format("%d", 4)"##;
        let mut h = blake3::Hasher::new();
        let tag = if opt_level() == 1 {
            b"rustudio bytecode 3|O1 D1 Vector3.new Vector3 mutable globals _G|".as_slice()
        } else {
            b"rustudio bytecode 3|O2 D1 Vector3.new Vector3 mutable globals _G|".as_slice()
        };
        h.update(tag);
        h.update(&compiler().compile(PROBE).unwrap_or_default());
        h.finalize().as_bytes().to_vec()
    })
}

struct Memory {
    map: HashMap<[u8; 32], Arc<Vec<u8>>>,
    bytes: usize,
}

fn memory() -> &'static Mutex<Memory> {
    static M: OnceLock<Mutex<Memory>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Memory { map: HashMap::new(), bytes: 0 }))
}

/// Drops all in-memory compiled bytecode entries and resets the byte counter
/// (e.g. on place switch or Android `onTrimMemory`).
pub fn clear_memory() {
    if let Ok(mut m) = memory().lock() {
        m.map.clear();
        m.map.shrink_to_fit();
        m.bytes = 0;
    }
}

fn dir() -> PathBuf {
    rustudio_core::assets::cache_dir().join("bytecode")
}

/// Bytecode for a script's source, compiled once per process (and kept on
/// disk for big sources). None: compile it the usual way (small, a syntax
/// error, or the cache is off).
pub fn get(source: &str) -> Option<Arc<Vec<u8>>> {
    // (A Source holding bytecode already is loaded as such by mlua.)
    if !enabled() || source.len() < MIN_MEMORY || source.as_bytes()[0] < 0x09 {
        return None;
    }
    let mut h = blake3::Hasher::new();
    h.update(salt());
    h.update(source.as_bytes());
    let key: [u8; 32] = *h.finalize().as_bytes();
    if let Some(b) = memory().lock().ok()?.map.get(&key) {
        return Some(b.clone());
    }
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let file = dir().join(format!("{hex}.luauc"));
    let disk = source.len() >= MIN_DISK;
    let from_disk = disk.then(|| std::fs::read(&file).ok()).flatten().filter(|b| b.first().is_some_and(|v| *v != 0));
    let bytes = match from_disk {
        Some(b) => b,
        None => {
            // A failed compile (or Luau's error bytecode, first byte 0) goes
            // the usual way, for the usual message.
            let b = compiler().compile(source).ok().filter(|b| b.first().is_some_and(|v| *v != 0))?;
            if disk {
                let _ = std::fs::create_dir_all(dir());
                let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
                let tmp = file.with_extension(format!("tmp.{}.{seq}", std::process::id()));
                if std::fs::write(&tmp, &b).is_ok() {
                    if std::fs::rename(&tmp, &file).is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
            b
        }
    };
    let b = Arc::new(bytes);
    if let Ok(mut m) = memory().lock() {
        if m.bytes + b.len() <= MEMORY_CAP {
            m.bytes += b.len();
            m.map.insert(key, b.clone());
        }
    }
    Some(b)
}

/// Collects all cache-eligible script `Source` strings from a `Document` for
/// background bytecode pre-compilation.
pub fn collect_script_sources(doc: &rustudio_core::doc::Document) -> Vec<String> {
    if !enabled() {
        return Vec::new();
    }
    let source_key = rbx_dom_weak::ustr("Source");
    let mut out = Vec::new();
    for inst in doc.dom.descendants() {
        if !matches!(inst.class.as_str(), "Script" | "LocalScript" | "ModuleScript") {
            continue;
        }
        if let Some(rbx_dom_weak::types::Variant::String(s)) = inst.properties.get(&source_key) {
            if s.len() >= MIN_MEMORY && s.as_bytes()[0] >= 0x09 {
                out.push(s.clone());
            }
        }
    }
    out
}

/// Pre-compiles `sources` into the bytecode cache on up to 2 low-priority
/// background threads, deduplicating and compiling the largest scripts first.
/// Calling `prewarm` again (e.g., when opening another place) cancels any
/// in-flight prewarm from a prior generation.
pub fn prewarm(sources: Vec<String>) {
    let gen = PREWARM_GEN.fetch_add(1, Ordering::SeqCst).wrapping_add(1);
    if !enabled() || sources.is_empty() {
        return;
    }
    let mut seen = rustc_hash::FxHashSet::default();
    let mut work: Vec<String> = Vec::with_capacity(sources.len());
    for s in sources {
        if s.len() < MIN_MEMORY || s.as_bytes()[0] < 0x09 {
            continue;
        }
        let hash = *blake3::hash(s.as_bytes()).as_bytes();
        if seen.insert(hash) {
            work.push(s);
        }
    }
    if work.is_empty() {
        return;
    }
    work.sort_unstable_by(|a, b| b.len().cmp(&a.len()));

    let avail = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    let threads = avail.saturating_sub(1).clamp(1, 2).min(work.len());
    let queue = Arc::new(work);
    let cursor = Arc::new(AtomicUsize::new(0));

    for t in 0..threads {
        let queue = Arc::clone(&queue);
        let cursor = Arc::clone(&cursor);
        let _ = std::thread::Builder::new()
            .name(format!("luau-prewarm-{t}"))
            .spawn(move || {
                #[cfg(any(target_os = "linux", target_os = "android"))]
                unsafe {
                    libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                }
                loop {
                    if PREWARM_GEN.load(Ordering::Acquire) != gen {
                        break;
                    }
                    let idx = cursor.fetch_add(1, Ordering::Relaxed);
                    if idx >= queue.len() {
                        break;
                    }
                    let _ = get(&queue[idx]);
                }
            });
    }
}
