//! Data files shipped inside the APK (`assets/`, listed under `assets:` in
//! crates/rustudio-android/manifest.yaml). The desktop build finds
//! `api-dump.json` next to its executable (`paths::resource`); here it is
//! copied out of the APK into the app's files folder once per APK version,
//! and `paths::resource` looks there.

use std::path::Path;

use super::AndroidApp;

/// The files to unpack, by their name in `assets/`.
const FILES: &[&str] = &["api-dump.json"];

/// Unpacks [`FILES`] into [`super::resource_dir`] when they are missing or
/// came from a different build of the app (the stamp holds the version).
pub fn unpack(app: &AndroidApp) -> Result<(), String> {
    let dir = super::resource_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp = dir.join(".apk-version");
    let version = env!("CARGO_PKG_VERSION");
    let fresh = std::fs::read_to_string(&stamp).is_ok_and(|v| v.trim() == version)
        && FILES.iter().all(|f| {
            std::fs::metadata(dir.join(f))
                .is_ok_and(|m| m.is_file() && m.len() >= 1_048_576)
        });
    if fresh {
        return Ok(());
    }
    let manager = app.asset_manager();
    for name in FILES {
        let cname = std::ffi::CString::new(*name).map_err(|e| e.to_string())?;
        let mut asset = manager.open(&cname).ok_or_else(|| format!("assets/{name} is not in the APK"))?;
        let expected = asset.length() as u64;
        let dest = dir.join(name);
        let tmp = dest.with_extension("part");
        let written = {
            let f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            let mut w = std::io::BufWriter::with_capacity(64 * 1024, f);
            let n = std::io::copy(&mut asset, &mut w).map_err(|e| format!("assets/{name}: {e}"))?;
            let f = w.into_inner().map_err(|e| format!("{}: {e}", tmp.display()))?;
            let _ = f.sync_all();
            n
        };
        if written < 1_048_576 || (expected > 0 && written != expected) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("assets/{name}: short unpack ({written} bytes, expected {expected})"));
        }
        std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        log::info!("assets: unpacked {name} ({written} bytes)");
    }
    write_atomically(&stamp, version.as_bytes())
}

/// Writes through a temporary file, so a kill mid-copy leaves either the
/// old file or the new one.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}
