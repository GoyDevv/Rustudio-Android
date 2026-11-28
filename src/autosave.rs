//! Per-place background auto-save and crash recovery.
//!
//! Snapshots live under `rustudio_core::paths::data().join("AutoSaves")` and
//! are keyed deterministically by the open document's origin:
//! - Roblox Cloud / Team Create place: `cloud_u{universe_id}_p{place_id}.rbxl` (+ `.meta.json`)
//! - Local file: `local_{hash64}.rbxl` (+ `.meta.json`)
//! - Untitled dirty place: `untitled_latest.rbxl` (+ `.meta.json`)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::RichText;
use rustudio_account::CloudOrigin;
use rustudio_core::doc::Document;
use serde::{Deserialize, Serialize};

use crate::app::Editor;

/// Serializes recovery file writes/clears so `flush_sync()` and background
/// `tick()` threads can never double-write or race on the same snapshot.
static WRITE_LOCK: Mutex<()> = Mutex::new(());
/// Monotonic sequence counter for unique temporary filenames.
static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Directory where per-place auto-recovery snapshots are stored.
pub fn dir() -> PathBuf {
    rustudio_core::paths::data().join("AutoSaves")
}

/// Deterministic 64-bit FNV-1a hash (stable across Rust versions and platforms).
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn hash_path(path: &Path) -> u64 {
    let norm = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    fnv1a_64(norm.to_string_lossy().as_bytes())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Deterministic identity for an auto-recovery snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryKey {
    Cloud { universe_id: u64, place_id: u64 },
    Local(PathBuf),
    Untitled,
}

impl RecoveryKey {
    pub fn for_doc(doc: &Document) -> Self {
        if let Some(origin) = doc.cloud.as_ref() {
            RecoveryKey::Cloud {
                universe_id: origin.universe_id,
                place_id: origin.place_id,
            }
        } else if let Some(path) = doc.path.as_ref() {
            RecoveryKey::Local(path.clone())
        } else {
            RecoveryKey::Untitled
        }
    }

    pub fn stem(&self) -> String {
        match self {
            RecoveryKey::Cloud { universe_id, place_id } => {
                format!("cloud_u{universe_id}_p{place_id}")
            }
            RecoveryKey::Local(path) => format!("local_{:016x}", hash_path(path)),
            RecoveryKey::Untitled => "untitled_latest".to_string(),
        }
    }

    pub fn rbxl_path(&self) -> PathBuf {
        dir().join(format!("{}.rbxl", self.stem()))
    }

    pub fn meta_path(&self) -> PathBuf {
        dir().join(format!("{}.meta.json", self.stem()))
    }

    fn tmp_rbxl_path(&self, seq: u64) -> PathBuf {
        dir().join(format!("{}.{seq}.rbxl.tmp", self.stem()))
    }

    fn tmp_meta_path(&self, seq: u64) -> PathBuf {
        dir().join(format!("{}.{seq}.meta.json.tmp", self.stem()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryMeta {
    pub title: String,
    pub saved_unix_secs: u64,
    pub universe_id: Option<u64>,
    pub place_id: Option<u64>,
    pub place_version: Option<u64>,
    pub local_path: Option<String>,
    pub revision: u64,
}

impl RecoveryMeta {
    pub fn for_doc(doc: &Document) -> Self {
        Self {
            title: if doc.title.is_empty() {
                "Untitled".to_string()
            } else {
                doc.title.clone()
            },
            saved_unix_secs: unix_now(),
            universe_id: doc.cloud.as_ref().map(|o| o.universe_id),
            place_id: doc.cloud.as_ref().map(|o| o.place_id),
            place_version: doc.cloud.as_ref().map(|o| o.version),
            local_path: doc.path.as_ref().map(|p| p.display().to_string()),
            revision: doc.revision,
        }
    }
}

/// Atomically writes the `.rbxl` snapshot and its `.meta.json` sidecar.
pub fn write_recovery(key: &RecoveryKey, bytes: &[u8], meta: &RecoveryMeta) -> std::io::Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let root = dir();
    std::fs::create_dir_all(&root)?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_rbxl = key.tmp_rbxl_path(seq);
    let dst_rbxl = key.rbxl_path();
    if let Err(e) = std::fs::write(&tmp_rbxl, bytes) {
        let _ = std::fs::remove_file(&tmp_rbxl);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp_rbxl, &dst_rbxl) {
        let _ = std::fs::remove_file(&tmp_rbxl);
        return Err(e);
    }

    if let Ok(json) = serde_json::to_string_pretty(meta) {
        let tmp_meta = key.tmp_meta_path(seq);
        let dst_meta = key.meta_path();
        if std::fs::write(&tmp_meta, json).is_ok() {
            if std::fs::rename(&tmp_meta, &dst_meta).is_err() {
                let _ = std::fs::remove_file(&tmp_meta);
            }
        } else {
            let _ = std::fs::remove_file(&tmp_meta);
        }
    }
    Ok(())
}

/// Deletes any recovery snapshot and metadata matching `key`.
pub fn clear_recovery(key: &RecoveryKey) {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = std::fs::remove_file(key.rbxl_path());
    let _ = std::fs::remove_file(key.meta_path());
    let stem = key.stem();
    let root = dir();
    let _ = std::fs::remove_file(root.join(format!("{stem}.rbxl.tmp")));
    let _ = std::fs::remove_file(root.join(format!("{stem}.meta.json.tmp")));
    if let Ok(entries) = std::fs::read_dir(&root) {
        let prefix = format!("{stem}.");
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && name.ends_with(".tmp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Deletes any recovery snapshot matching `doc`.
pub fn clear_for_doc(doc: &Document) {
    clear_recovery(&RecoveryKey::for_doc(doc));
}

/// Checks whether a non-empty recovery file exists for `key`.
pub fn find_recovery(key: &RecoveryKey) -> Option<(PathBuf, Option<RecoveryMeta>)> {
    let rbxl = key.rbxl_path();
    let file_meta = std::fs::metadata(&rbxl).ok()?;
    if !file_meta.is_file() || file_meta.len() == 0 {
        return None;
    }
    let meta = std::fs::read_to_string(key.meta_path())
        .ok()
        .and_then(|s| serde_json::from_str::<RecoveryMeta>(&s).ok());
    Some((rbxl, meta))
}

#[derive(Debug, Clone)]
pub enum RecoveryTarget {
    Cloud(CloudOrigin),
    Local(PathBuf),
}

#[derive(Debug, Clone)]
pub struct PendingRecovery {
    pub key: RecoveryKey,
    pub rbxl_path: PathBuf,
    pub title: String,
    pub saved_unix_secs: Option<u64>,
    pub target: RecoveryTarget,
}

/// Per-editor auto-save scheduler and crash-recovery prompt state.
pub struct AutoSaveState {
    last_saved_doc_id: u64,
    last_saved_revision: u64,
    last_attempt: Instant,
    in_flight: Arc<AtomicBool>,
    pub pending_recovery: Option<PendingRecovery>,
}

impl Default for AutoSaveState {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoSaveState {
    pub fn new() -> Self {
        Self {
            last_saved_doc_id: 0,
            last_saved_revision: 0,
            last_attempt: Instant::now(),
            in_flight: Arc::new(AtomicBool::new(false)),
            pending_recovery: None,
        }
    }

    /// Marks the given `(doc_id, revision)` as already snapshotted so the next
    /// tick waits for a new edit before saving again.
    pub fn note_saved(&mut self, doc_id: u64, revision: u64) {
        self.last_saved_doc_id = doc_id;
        self.last_saved_revision = revision;
        self.last_attempt = Instant::now();
    }

    /// Periodic per-frame tick (called from `RustudioApp::frame()`).
    /// Serializes dirty places and writes recovery files on a background thread.
    pub fn tick(&mut self, doc: &Document, playing: bool) {
        if !rustudio_core::rustudio_settings::autosave_enabled() {
            return;
        }
        if playing || self.pending_recovery.is_some() || !doc.dirty() {
            return;
        }
        if (doc.id, doc.revision) == (self.last_saved_doc_id, self.last_saved_revision) {
            return;
        }
        let interval = Duration::from_secs(rustudio_core::rustudio_settings::autosave_interval_secs());
        if self.last_attempt.elapsed() < interval {
            return;
        }
        if self.in_flight.load(Ordering::Acquire) {
            return;
        }

        self.last_attempt = Instant::now();
        let bytes = match doc.encode_all(false) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("autosave: encode failed: {e}");
                return;
            }
        };

        let key = RecoveryKey::for_doc(doc);
        let meta = RecoveryMeta::for_doc(doc);
        self.last_saved_doc_id = doc.id;
        self.last_saved_revision = doc.revision;
        self.in_flight.store(true, Ordering::Release);

        let in_flight = Arc::clone(&self.in_flight);
        let spawned = std::thread::Builder::new()
            .name("autosave".into())
            .spawn(move || {
                if let Err(e) = write_recovery(&key, &bytes, &meta) {
                    log::warn!("autosave: write failed ({}): {e}", key.stem());
                }
                in_flight.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.in_flight.store(false, Ordering::Release);
        }
    }

    /// Synchronous flush for lifecycle pause / Android `eframe::App::save()`.
    pub fn flush_sync(&mut self, doc: &Document, playing: bool) {
        if !rustudio_core::rustudio_settings::autosave_enabled() {
            return;
        }
        if playing || self.pending_recovery.is_some() || !doc.dirty() {
            return;
        }
        if (doc.id, doc.revision) == (self.last_saved_doc_id, self.last_saved_revision) {
            return;
        }
        if let Ok(bytes) = doc.encode_all(false) {
            let key = RecoveryKey::for_doc(doc);
            let meta = RecoveryMeta::for_doc(doc);
            if write_recovery(&key, &bytes, &meta).is_ok() {
                self.last_saved_doc_id = doc.id;
                self.last_saved_revision = doc.revision;
                self.last_attempt = Instant::now();
            }
        }
    }

    /// Checks for a matching crash-recovery file after opening a Roblox Cloud place.
    pub fn check_cloud_recovery(&mut self, origin: &CloudOrigin) {
        if !rustudio_core::rustudio_settings::autosave_enabled() {
            return;
        }
        let key = RecoveryKey::Cloud {
            universe_id: origin.universe_id,
            place_id: origin.place_id,
        };
        if let Some((rbxl_path, meta)) = find_recovery(&key) {
            let title = meta
                .as_ref()
                .map(|m| m.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| origin.name.clone());
            let saved_unix_secs = meta.map(|m| m.saved_unix_secs).filter(|&s| s > 0);
            self.pending_recovery = Some(PendingRecovery {
                key,
                rbxl_path,
                title,
                saved_unix_secs,
                target: RecoveryTarget::Cloud(origin.clone()),
            });
        }
    }

    /// Checks for a matching crash-recovery file after opening a local place file.
    pub fn check_local_recovery(&mut self, path: &Path) {
        if !rustudio_core::rustudio_settings::autosave_enabled() {
            return;
        }
        let key = RecoveryKey::Local(path.to_path_buf());
        if let Some((rbxl_path, meta)) = find_recovery(&key) {
            let title = meta
                .as_ref()
                .map(|m| m.title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                });
            let saved_unix_secs = meta.map(|m| m.saved_unix_secs).filter(|&s| s > 0);
            self.pending_recovery = Some(PendingRecovery {
                key,
                rbxl_path,
                title,
                saved_unix_secs,
                target: RecoveryTarget::Local(path.to_path_buf()),
            });
        }
    }
}

fn format_age(saved_unix_secs: u64) -> String {
    let now = unix_now();
    let diff = now.saturating_sub(saved_unix_secs);
    if diff < 5 {
        "just now".to_string()
    } else if diff < 60 {
        format!("{diff}s ago")
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86_400)
    }
}

impl Editor {
    /// Mobile-clamped Auto-Recovery modal shown when a matching recovery snapshot
    /// is found on reopen of a Roblox Cloud place or local file.
    pub fn autosave_recovery_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.autosave.pending_recovery.clone() else {
            return;
        };
        let mut action: Option<bool> = None; // Some(true) = recover, Some(false) = discard
        let screen = ctx.content_rect();
        let max_w = (screen.width() - 32.0).clamp(260.0, 460.0);
        let max_h = (screen.height() - 110.0).max(120.0);

        egui::Modal::new(egui::Id::new("autosave_recovery_modal")).show(ctx, |ui| {
            ui.set_max_width(max_w);
            egui::ScrollArea::vertical()
                .id_salt("autosave_recovery_scroll")
                .max_height(max_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(RichText::new("Auto-Recovery Available").strong().size(15.0));
                    ui.add_space(4.0);
                    let source_desc = match &pending.target {
                        RecoveryTarget::Cloud(origin) => {
                            format!("{} (Roblox Cloud, place {})", pending.title, origin.place_id)
                        }
                        RecoveryTarget::Local(path) => {
                            format!("{} ({})", pending.title, path.display())
                        }
                    };
                    ui.label(RichText::new(source_desc).strong());
                    if let Some(ts) = pending.saved_unix_secs {
                        ui.label(
                            RichText::new(format!("Auto-saved {}", format_age(ts)))
                                .small()
                                .color(rustudio_engine::theme::TEXT_DIM),
                        );
                    }
                    ui.add_space(6.0);
                    ui.label(
                        "An unsaved auto-recovery snapshot was found for this place. \
                         Recover your unsaved changes, or discard the snapshot to keep the opened copy.",
                    );
                });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Recover Changes").clicked() {
                    action = Some(true);
                }
                if ui.button("Discard").clicked() {
                    action = Some(false);
                }
            });
        });

        match action {
            Some(true) => {
                self.autosave.pending_recovery = None;
                self.apply_recovery(pending);
            }
            Some(false) => {
                clear_recovery(&pending.key);
                self.autosave.pending_recovery = None;
                self.info("Discarded auto-recovery snapshot");
            }
            None => {}
        }
    }

    fn apply_recovery(&mut self, pending: PendingRecovery) {
        let bytes = match std::fs::read(&pending.rbxl_path) {
            Ok(b) => b,
            Err(e) => {
                self.error(format!("Could not read auto-recovery file: {e}"));
                return;
            }
        };
        match pending.target {
            RecoveryTarget::Cloud(origin) => {
                let title = if origin.local_copy {
                    format!("{} (local copy)", origin.name)
                } else {
                    origin.name.clone()
                };
                match Document::from_place_bytes(&bytes, title) {
                    Ok(mut d) => {
                        let root = d.dom.root_ref();
                        if let Some(inst) = d.dom.get_by_ref_mut(root) {
                            use rbx_dom_weak::{
                                types::{Enum, Variant},
                                ustr,
                            };
                            inst.properties.insert(ustr("PlaceId"), Variant::Int64(origin.place_id as i64));
                            inst.properties.insert(ustr("GameId"), Variant::Int64(origin.universe_id as i64));
                            inst.properties.insert(ustr("CreatorId"), Variant::Int64(origin.creator_id as i64));
                            inst.properties.insert(
                                ustr("CreatorType"),
                                Variant::Enum(Enum::from_u32(origin.creator_type.enum_value())),
                            );
                            inst.properties.insert(ustr("PlaceVersion"), Variant::Int32(origin.version as i32));
                        }
                        d.cloud = Some(origin.clone());
                        // Mark dirty so saving updates Roblox Cloud.
                        d.mark_saved_at(0);
                        let doc_id = d.id;
                        let doc_rev = d.revision;
                        let n = d.dom.descendants().count();
                        self.set_document(d);
                        self.autosave.note_saved(doc_id, doc_rev);
                        self.info(format!(
                            "Recovered unsaved changes for {} ({n} instances)",
                            origin.name
                        ));
                    }
                    Err(e) => self.error(format!("Could not load auto-recovery snapshot: {e}")),
                }
            }
            RecoveryTarget::Local(path) => {
                let title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match Document::from_place_bytes(&bytes, title) {
                    Ok(mut d) => {
                        d.path = Some(path.clone());
                        // Mark dirty so saving updates the local file.
                        d.mark_saved_at(0);
                        let doc_id = d.id;
                        let doc_rev = d.revision;
                        let n = d.dom.descendants().count();
                        self.set_document(d);
                        self.autosave.note_saved(doc_id, doc_rev);
                        self.info(format!(
                            "Recovered unsaved changes for {} ({n} instances)",
                            path.display()
                        ));
                    }
                    Err(e) => self.error(format!("Could not load auto-recovery snapshot: {e}")),
                }
            }
        }
    }
}
