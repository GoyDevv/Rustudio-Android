//! Files on Android: the app's own folder in shared storage
//! (`Android/data/<package>/files`, visible to file managers, no permission
//! needed), a Godot-style in-app file picker drawn in egui ([`Picker`], behind
//! `crate::file_dialog::Dialogs`) with async RAM-capped thumbnails and preview,
//! Android's System Document Picker (SAF `ACTION_OPEN_DOCUMENT` routed directly
//! back to the waiting [`Picker`]), and places handed over by other apps
//! through "Open with" (a `content:` URI in the launch Intent, copied in
//! through ContentResolver: [`launch_file`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};

use crate::file_dialog::{Kind, Request};

// ------------------------------------------------------------------ Palette

const BG_WINDOW: Color32 = Color32::from_rgb(29, 32, 38);
const BG_PANEL: Color32 = Color32::from_rgb(23, 26, 31);
const BG_BROWSER: Color32 = Color32::from_rgb(20, 23, 28);
const BG_TILE: Color32 = Color32::from_rgb(28, 32, 39);
const BG_HOVER: Color32 = Color32::from_rgb(39, 45, 56);
const BG_SELECTED: Color32 = Color32::from_rgb(36, 59, 85);
const BORDER: Color32 = Color32::from_rgb(45, 51, 63);
const ACCENT: Color32 = Color32::from_rgb(56, 122, 235);
const ACCENT_SOFT: Color32 = Color32::from_rgb(39, 59, 84);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(232, 236, 242);
const TEXT_MUTED: Color32 = Color32::from_rgb(142, 151, 166);
const FOLDER_BODY: Color32 = Color32::from_rgb(229, 181, 92);
const FOLDER_TAB: Color32 = Color32::from_rgb(198, 148, 60);
const WARN_BG: Color32 = Color32::from_rgb(54, 43, 24);
const WARN_BORDER: Color32 = Color32::from_rgb(156, 118, 48);
const WARN_TEXT: Color32 = Color32::from_rgb(245, 212, 138);

// ------------------------------------------------------------ Thumbnail worker

const MAX_THUMB_CACHE: usize = 48;
const THUMB_MAX_DIM: u32 = 96;
const PREVIEW_MAX_DIM: u32 = 150;
const MAX_THUMB_FILE_BYTES: u64 = 24 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    List,
    Grid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileBadge {
    Img,
    Model3D,
    Audio,
    Roblox,
    Lua,
    File,
}

impl FileBadge {
    fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tga" | "gif" => Self::Img,
            "glb" | "gltf" | "obj" | "fbx" | "stl" | "dae" => Self::Model3D,
            "mp3" | "ogg" | "wav" | "flac" | "m4a" => Self::Audio,
            "rbxl" | "rbxlx" | "rbxm" | "rbxmx" => Self::Roblox,
            "lua" | "luau" => Self::Lua,
            _ => Self::File,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Img => "IMG",
            Self::Model3D => "3D",
            Self::Audio => "AUD",
            Self::Roblox => "RBX",
            Self::Lua => "LUA",
            Self::File => "FILE",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Img => Color32::from_rgb(78, 201, 176),
            Self::Model3D => Color32::from_rgb(197, 134, 192),
            Self::Audio => Color32::from_rgb(220, 205, 130),
            Self::Roblox => Color32::from_rgb(86, 156, 214),
            Self::Lua => Color32::from_rgb(79, 193, 255),
            Self::File => Color32::from_rgb(140, 149, 166),
        }
    }

    fn bg(self) -> Color32 {
        let c = self.color();
        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 34)
    }

    fn is_image(self) -> bool {
        self == Self::Img
    }
}

struct ThumbReq {
    path: PathBuf,
    max_dim: u32,
    generation: u64,
    is_preview: bool,
    ctx: egui::Context,
}

struct ThumbResp {
    path: PathBuf,
    generation: u64,
    is_preview: bool,
    decoded: Option<(egui::ColorImage, u32, u32)>,
}

enum ThumbEntry {
    Loaded { tex: egui::TextureHandle, dims: (u32, u32) },
    Failed,
}

struct PreviewTex {
    path: PathBuf,
    tex: Option<egui::TextureHandle>,
    dims: Option<(u32, u32)>,
}

fn spawn_thumb_worker() -> (Sender<ThumbReq>, Receiver<ThumbResp>) {
    let (req_tx, req_rx) = mpsc::channel::<ThumbReq>();
    let (resp_tx, resp_rx) = mpsc::channel::<ThumbResp>();
    let _ = std::thread::Builder::new().name("android-picker-thumbs".into()).spawn(move || {
        while let Ok(req) = req_rx.recv() {
            let decoded = decode_thumbnail(&req.path, req.max_dim);
            let ctx = req.ctx;
            if resp_tx
                .send(ThumbResp {
                    path: req.path,
                    generation: req.generation,
                    is_preview: req.is_preview,
                    decoded,
                })
                .is_err()
            {
                break;
            }
            ctx.request_repaint();
        }
    });
    (req_tx, resp_rx)
}

fn decode_thumbnail(path: &Path, max_dim: u32) -> Option<(egui::ColorImage, u32, u32)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_THUMB_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let dyn_img = image::load_from_memory(&bytes).ok()?;
    let orig_w = dyn_img.width();
    let orig_h = dyn_img.height();
    let resized = dyn_img.thumbnail(max_dim, max_dim).to_rgba8();
    let size = [resized.width() as usize, resized.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    Some((egui::ColorImage::from_rgba_unmultiplied(size, resized.as_raw()), orig_w, orig_h))
}

// ------------------------------------------------------------------- Roots

/// A folder the picker can start from.
struct Root {
    label: &'static str,
    path: PathBuf,
}

/// The primary shared storage (`/storage/emulated/0`): the app's external
/// folder is `<shared>/Android/data/<package>/files`.
fn shared_storage() -> PathBuf {
    let mut p = super::data_dir();
    for _ in 0..4 {
        if let Some(parent) = p.parent() {
            p = parent.to_path_buf();
        }
    }
    if p.as_os_str().is_empty() || p == Path::new("/") {
        PathBuf::from("/storage/emulated/0")
    } else {
        p
    }
}

fn roots(_storage_access: bool) -> Vec<Root> {
    let mut v = vec![Root { label: "Rustudio", path: super::data_dir() }];
    let imported = super::data_dir().join("Imported");
    let _ = std::fs::create_dir_all(&imported);
    v.push(Root { label: "Imported", path: imported });
    if super::data_dir() != super::files_dir() {
        v.push(Root { label: "App data", path: super::files_dir() });
    }
    let shared = shared_storage();
    for (label, sub) in [
        ("Download", "Download"),
        ("Pictures", "Pictures"),
        ("DCIM", "DCIM"),
        ("Documents", "Documents"),
        ("Music", "Music"),
    ] {
        let p = shared.join(sub);
        if p.is_dir() && std::fs::read_dir(&p).is_ok() {
            v.push(Root { label, path: p });
        }
    }
    if shared.is_dir() && std::fs::read_dir(&shared).is_ok() {
        v.push(Root { label: "Shared storage", path: shared });
    }
    v
}

// ----------------------------------------------------------------- Listing

struct FolderEntry {
    path: PathBuf,
    name: String,
}

struct FileEntry {
    path: PathBuf,
    name: String,
    size: u64,
    badge: FileBadge,
}

/// One folder's listing, sorted folders first.
struct Listing {
    dir: PathBuf,
    folders: Vec<FolderEntry>,
    files: Vec<FileEntry>,
    hidden_by_filter: usize,
}

fn request_accepts_case_insensitive(request: &Request, path: &Path) -> bool {
    if request.filters.is_empty() {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext.is_empty() {
        return false;
    }
    request.filters.iter().any(|(_, exts)| {
        exts.iter().any(|f| {
            let f_low = f.trim_start_matches('.').to_ascii_lowercase();
            if f_low == "*" || f_low == ext {
                return true;
            }
            // Treat jpg/jpeg and tif/tiff as equivalent so camera photos are never missed.
            matches!(
                (f_low.as_str(), ext.as_str()),
                ("jpg", "jpeg") | ("jpeg", "jpg") | ("tif", "tiff") | ("tiff", "tif")
            )
        })
    })
}

impl Listing {
    fn read(dir: &Path, request: &Request, show_all_files: bool) -> Self {
        let mut folders = Vec::new();
        let mut files = Vec::new();
        let mut hidden_by_filter = 0usize;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if name.is_empty() || name.starts_with('.') {
                    continue;
                }
                // Follow symlinks via metadata() first, falling back to DirEntry::file_type().
                let meta = std::fs::metadata(&p).ok();
                let is_dir = meta
                    .as_ref()
                    .map(|m| m.is_dir())
                    .unwrap_or_else(|| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false));
                if is_dir {
                    folders.push(FolderEntry { path: p, name });
                } else if show_all_files || request_accepts_case_insensitive(request, &p) {
                    let size = meta.map(|m| m.len()).unwrap_or(0);
                    let badge = FileBadge::from_path(&p);
                    files.push(FileEntry { path: p, name, size, badge });
                } else {
                    hidden_by_filter += 1;
                }
            }
        }
        folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Listing { dir: dir.to_path_buf(), folders, files, hidden_by_filter }
    }
}

// --------------------------------------------------- System Picker (SAF)

const SAF_OPEN_DOC_REQUEST_CODE: i32 = 0x5254;

/// Dedicated result slot for Android's SAF System Picker (`REQ_SAF_OPEN_DOC`).
/// Completely separate from [`OPENED`] (which handles external "Open with" VIEW
/// intents) so picking images/models/audio never triggers place-open prompts.
static SAF_PICKER_RESULT: std::sync::Mutex<Option<Vec<PathBuf>>> = std::sync::Mutex::new(None);

fn clear_saf_picker_result() {
    *SAF_PICKER_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn set_saf_picker_result(paths: Vec<PathBuf>) {
    *SAF_PICKER_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(paths);
}

fn take_saf_picker_result() -> Option<Vec<PathBuf>> {
    SAF_PICKER_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take()
}

fn saf_mime_for_request(request: &Request) -> &'static str {
    if request.filters.is_empty() {
        return "*/*";
    }
    let mut saw_any = false;
    let mut all_images = true;
    let mut all_audio = true;
    for (_, exts) in &request.filters {
        for ext in exts {
            saw_any = true;
            let low = ext.trim_start_matches('.').to_ascii_lowercase();
            if !matches!(low.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tga") {
                all_images = false;
            }
            if !matches!(low.as_str(), "mp3" | "ogg" | "wav" | "flac" | "m4a") {
                all_audio = false;
            }
        }
    }
    if saw_any && all_images {
        "image/*"
    } else if saw_any && all_audio {
        "audio/*"
    } else {
        "*/*"
    }
}

/// Launches Android's Storage Access Framework document picker
/// (`ACTION_OPEN_DOCUMENT`, `CATEGORY_OPENABLE`).
fn open_system_picker(multi: bool, request: &Request) -> super::jni::Result<()> {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    let mime_str = saf_mime_for_request(request);
    super::jni::with_activity(|env, activity| {
        let action = env.new_string("android.intent.action.OPEN_DOCUMENT")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )?;
        let category = env.new_string("android.intent.category.OPENABLE")?;
        env.call_method(
            &intent,
            jni_str!("addCategory"),
            jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&category)],
        )?;
        let mime = env.new_string(mime_str)?;
        env.call_method(
            &intent,
            jni_str!("setType"),
            jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&mime)],
        )?;
        // FLAG_GRANT_READ_URI_PERMISSION = 0x1
        env.call_method(
            &intent,
            jni_str!("addFlags"),
            jni_sig!("(I)Landroid/content/Intent;"),
            &[JValue::Int(0x0000_0001)],
        )?;
        if multi {
            let extra_multi = env.new_string("android.intent.extra.ALLOW_MULTIPLE")?;
            env.call_method(
                &intent,
                jni_str!("putExtra"),
                jni_sig!("(Ljava/lang/String;Z)Landroid/content/Intent;"),
                &[JValue::Object(&extra_multi), JValue::Bool(true)],
            )?;
        }
        env.call_method(
            activity,
            jni_str!("startActivityForResult"),
            jni_sig!("(Landroid/content/Intent;I)V"),
            &[JValue::Object(&intent), JValue::Int(SAF_OPEN_DOC_REQUEST_CODE)],
        )?;
        Ok(())
    })
}

// ------------------------------------------------------------------ Picker

/// The in-app Godot-style file dialog.
pub struct Picker {
    request: Request,
    kind: Kind,
    multi: bool,
    show_all_files: bool,
    view_mode: ViewMode,
    listing: Listing,
    selected: Option<PathBuf>,
    selected_multi: Vec<PathBuf>,
    /// Save: the name to write; new folder: its name.
    name: String,
    new_folder: Option<String>,
    error: Option<String>,
    waiting_system_picker: bool,
    /// Shared storage may be read (asked again now and then while the
    /// dialog is open: the grant happens on a system page).
    storage_access: bool,
    access_checked: Instant,
    // Async RAM-capped thumbnail state
    thumb_tx: Sender<ThumbReq>,
    thumb_rx: Receiver<ThumbResp>,
    thumb_generation: u64,
    thumbs: HashMap<PathBuf, ThumbEntry>,
    thumb_lru: VecDeque<PathBuf>,
    thumb_inflight: HashSet<PathBuf>,
    preview_tex: Option<PreviewTex>,
    preview_inflight: Option<PathBuf>,
}

impl Drop for Picker {
    fn drop(&mut self) {
        self.clear_thumbnails();
    }
}

impl Picker {
    pub fn new(request: Request, kind: Kind) -> Self {
        Self::new_with_mode(request, kind, false)
    }

    pub fn new_multi(request: Request) -> Self {
        Self::new_with_mode(request, Kind::PickFile, true)
    }

    fn new_with_mode(request: Request, kind: Kind, multi: bool) -> Self {
        let start = request
            .directory
            .clone()
            .filter(|d| d.is_dir())
            .unwrap_or_else(|| {
                let d = super::data_dir().join("Places");
                let _ = std::fs::create_dir_all(&d);
                d
            });
        let mut name = request.file_name.clone().unwrap_or_default();
        if kind != Kind::SaveFile {
            name.clear();
        }
        let show_all_files = request.filters.is_empty();
        let (thumb_tx, thumb_rx) = spawn_thumb_worker();
        Picker {
            listing: Listing::read(&start, &request, show_all_files),
            request,
            kind,
            multi,
            show_all_files,
            view_mode: ViewMode::List,
            selected: None,
            selected_multi: Vec::new(),
            name,
            new_folder: None,
            error: None,
            waiting_system_picker: false,
            storage_access: super::jni::has_storage_access(),
            access_checked: Instant::now(),
            thumb_tx,
            thumb_rx,
            thumb_generation: 1,
            thumbs: HashMap::new(),
            thumb_lru: VecDeque::new(),
            thumb_inflight: HashSet::new(),
            preview_tex: None,
            preview_inflight: None,
        }
    }

    fn clear_thumbnails(&mut self) {
        self.thumb_generation = self.thumb_generation.wrapping_add(1);
        self.thumbs.clear();
        self.thumb_lru.clear();
        self.thumb_inflight.clear();
        self.preview_tex = None;
        self.preview_inflight = None;
    }

    fn poll_thumbnails(&mut self, ctx: &egui::Context) {
        while let Ok(resp) = self.thumb_rx.try_recv() {
            if resp.generation != self.thumb_generation {
                continue;
            }
            if resp.is_preview {
                if self.preview_inflight.as_ref() == Some(&resp.path) {
                    self.preview_inflight = None;
                }
                if self.active_preview_path().as_deref() == Some(resp.path.as_path()) {
                    let (tex, dims) = match resp.decoded {
                        Some((img, w, h)) => {
                            let handle = ctx.load_texture(
                                format!("android-picker-preview-{}", self.thumb_generation),
                                img,
                                egui::TextureOptions::LINEAR,
                            );
                            (Some(handle), Some((w, h)))
                        }
                        None => (None, None),
                    };
                    self.preview_tex = Some(PreviewTex { path: resp.path, tex, dims });
                }
                continue;
            }
            self.thumb_inflight.remove(&resp.path);
            match resp.decoded {
                Some((img, w, h)) => {
                    // Enforce strict 48-texture RAM cap.
                    while self.thumb_lru.len() >= MAX_THUMB_CACHE {
                        if let Some(evict) = self.thumb_lru.pop_front() {
                            self.thumbs.remove(&evict);
                        }
                    }
                    let tex = ctx.load_texture(
                        format!("android-picker-thumb-{}-{}", self.thumb_generation, self.thumb_lru.len()),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    self.thumb_lru.push_back(resp.path.clone());
                    self.thumbs.insert(resp.path, ThumbEntry::Loaded { tex, dims: (w, h) });
                }
                None => {
                    self.thumbs.insert(resp.path, ThumbEntry::Failed);
                }
            }
        }
    }

    fn request_thumb_if_needed(&mut self, ctx: &egui::Context, path: &Path) {
        if self.thumbs.contains_key(path) || self.thumb_inflight.contains(path) {
            return;
        }
        // Keep in-flight queue bounded so rapid scrolling doesn't queue hundreds of images.
        if self.thumb_inflight.len() >= 12 {
            return;
        }
        self.thumb_inflight.insert(path.to_path_buf());
        let _ = self.thumb_tx.send(ThumbReq {
            path: path.to_path_buf(),
            max_dim: THUMB_MAX_DIM,
            generation: self.thumb_generation,
            is_preview: false,
            ctx: ctx.clone(),
        });
    }

    fn request_preview_if_needed(&mut self, ctx: &egui::Context, path: &Path) {
        if self.preview_tex.as_ref().is_some_and(|p| p.path == path) {
            return;
        }
        if self.preview_inflight.as_deref() == Some(path) {
            return;
        }
        self.preview_tex = None;
        self.preview_inflight = Some(path.to_path_buf());
        let _ = self.thumb_tx.send(ThumbReq {
            path: path.to_path_buf(),
            max_dim: PREVIEW_MAX_DIM,
            generation: self.thumb_generation,
            is_preview: true,
            ctx: ctx.clone(),
        });
    }

    fn active_preview_path(&self) -> Option<PathBuf> {
        if self.multi {
            self.selected.clone().or_else(|| self.selected_multi.last().cloned())
        } else {
            self.selected.clone()
        }
    }

    /// Re-asks the system about storage access about once a second.
    fn poll_access(&mut self, ctx: &egui::Context) {
        if self.access_checked.elapsed().as_secs_f32() > 1.0 {
            self.access_checked = Instant::now();
            let now = super::jni::has_storage_access();
            if now && !self.storage_access {
                let dir = self.listing.dir.clone();
                self.go(&dir);
            }
            self.storage_access = now;
        }
        if !self.storage_access {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn go(&mut self, dir: &Path) {
        self.clear_thumbnails();
        self.listing = Listing::read(dir, &self.request, self.show_all_files);
        self.selected = None;
        self.error = None;
    }

    fn refresh_listing(&mut self) {
        let dir = self.listing.dir.clone();
        self.listing = Listing::read(&dir, &self.request, self.show_all_files);
    }

    /// The answer when the single-file dialog closes: `Some(Some(path))` chosen,
    /// `Some(None)` cancelled, `None` while it stays open.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Option<PathBuf>> {
        self.show_inner(ctx).map(|opt| opt.and_then(|mut v| v.pop()))
    }

    /// The answer when the multi-file dialog closes: `Some(paths)` chosen
    /// (empty when cancelled), `None` while it stays open.
    pub fn show_multi(&mut self, ctx: &egui::Context) -> Option<Vec<PathBuf>> {
        self.show_inner(ctx).map(|opt| opt.unwrap_or_default())
    }

    fn show_inner(&mut self, ctx: &egui::Context) -> Option<Option<Vec<PathBuf>>> {
        // 1. Check if Android's System Picker (SAF) returned a result while paused/resumed.
        if let Some(paths) = take_saf_picker_result() {
            self.waiting_system_picker = false;
            if !paths.is_empty() {
                self.clear_thumbnails();
                let picked = if self.multi { paths } else { vec![paths[0].clone()] };
                return Some(Some(picked));
            }
        }
        if self.waiting_system_picker {
            ctx.request_repaint_after(Duration::from_millis(120));
        }

        self.poll_access(ctx);
        self.poll_thumbnails(ctx);

        let title = self.request.title.clone().unwrap_or_else(|| {
            match self.kind {
                Kind::PickFile if self.multi => "Import Files",
                Kind::PickFile => "Open File",
                Kind::PickFolder => "Choose Folder",
                Kind::SaveFile => "Save As",
            }
            .to_string()
        });

        let roots = roots(self.storage_access);
        let avail = ctx.content_rect().shrink(12.0);
        let win_size = Vec2::new(avail.width().min(860.0).max(300.0), avail.height().min(580.0).max(260.0));
        let mut result: Option<Option<Vec<PathBuf>>> = None;
        let mut open = true;
        let mut nav_dir: Option<PathBuf> = None;

        egui::Window::new(RichText::new(title).strong().color(TEXT_PRIMARY))
            .id(egui::Id::new("android-file-picker"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .fixed_size(win_size)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(
                egui::Frame::new()
                    .fill(BG_WINDOW)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(10)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                ui.spacing_mut().button_padding = Vec2::new(9.0, 5.0);

                // ==================== 1. TOP TOOLBAR ====================
                ui.horizontal(|ui| {
                    let up = self.listing.dir.parent().map(Path::to_path_buf);
                    let at_root = roots.iter().any(|r| r.path == self.listing.dir)
                        || self.listing.dir == Path::new("/");
                    if ui
                        .add_enabled(
                            up.is_some() && !at_root,
                            egui::Button::new(RichText::new("↑ Up").size(12.5).color(TEXT_PRIMARY))
                                .fill(BG_TILE)
                                .min_size(Vec2::new(48.0, 28.0)),
                        )
                        .clicked()
                    {
                        if let Some(u) = up {
                            nav_dir = Some(u);
                        }
                    }

                    if ui
                        .add(
                            egui::Button::new(RichText::new("Home").size(12.5).color(TEXT_PRIMARY))
                                .fill(BG_TILE)
                                .min_size(Vec2::new(48.0, 28.0)),
                        )
                        .clicked()
                    {
                        nav_dir = Some(super::data_dir());
                    }

                    if ui
                        .add(
                            egui::Button::new(RichText::new("+ Folder").size(12.5).color(TEXT_PRIMARY))
                                .fill(BG_TILE)
                                .min_size(Vec2::new(66.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.new_folder = Some(String::new());
                    }

                    // Right-aligned view toggle & System Picker accent button.
                    let right_controls_w = if self.kind == Kind::PickFile { 206.0 } else { 94.0 };
                    let path_w = (ui.available_width() - right_controls_w).max(60.0);

                    // Path / breadcrumb bar
                    let (path_rect, _) = ui.allocate_exact_size(Vec2::new(path_w, 28.0), Sense::hover());
                    ui.painter().rect_filled(path_rect, 5.0, BG_BROWSER);
                    ui.painter().rect_stroke(path_rect, 5.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
                    let path_text = shorten(&self.listing.dir);
                    ui.painter().with_clip_rect(path_rect.shrink2(Vec2::new(8.0, 2.0))).text(
                        Pos2::new(path_rect.left() + 8.0, path_rect.center().y),
                        Align2::LEFT_CENTER,
                        path_text,
                        FontId::proportional(12.0),
                        TEXT_MUTED,
                    );

                    // List / Grid toggle
                    let list_active = self.view_mode == ViewMode::List;
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("List").size(12.0).color(if list_active { Color32::WHITE } else { TEXT_MUTED }),
                            )
                            .fill(if list_active { ACCENT_SOFT } else { BG_TILE })
                            .stroke(Stroke::new(1.0, if list_active { ACCENT } else { BORDER }))
                            .min_size(Vec2::new(40.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.view_mode = ViewMode::List;
                    }
                    let grid_active = self.view_mode == ViewMode::Grid;
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Grid").size(12.0).color(if grid_active { Color32::WHITE } else { TEXT_MUTED }),
                            )
                            .fill(if grid_active { ACCENT_SOFT } else { BG_TILE })
                            .stroke(Stroke::new(1.0, if grid_active { ACCENT } else { BORDER }))
                            .min_size(Vec2::new(40.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.view_mode = ViewMode::Grid;
                    }

                    if self.kind == Kind::PickFile {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("System Picker").size(12.0).strong().color(Color32::WHITE))
                                    .fill(ACCENT)
                                    .min_size(Vec2::new(104.0, 28.0)),
                            )
                            .clicked()
                        {
                            clear_saf_picker_result();
                            match open_system_picker(self.multi, &self.request) {
                                Ok(()) => {
                                    self.waiting_system_picker = true;
                                    self.error = None;
                                }
                                Err(e) => {
                                    self.waiting_system_picker = false;
                                    self.error = Some(format!("Could not open system picker: {e}"));
                                }
                            }
                        }
                    }
                });

                // Compact All Files Access banner when Android Scoped Storage may hide files
                if !self.storage_access {
                    egui::Frame::new()
                        .fill(WARN_BG)
                        .stroke(Stroke::new(1.0, WARN_BORDER))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(
                                        "Some files may be hidden by Android — Grant All Files Access or use System Picker.",
                                    )
                                    .size(11.5)
                                    .color(WARN_TEXT),
                                );
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("Grant All Files Access").size(11.5).strong().color(Color32::WHITE),
                                        )
                                        .fill(Color32::from_rgb(168, 114, 30))
                                        .min_size(Vec2::new(0.0, 22.0)),
                                    )
                                    .clicked()
                                {
                                    if let Err(e) = super::jni::request_storage_access() {
                                        self.error = Some(format!("Could not open permission page: {e}"));
                                    }
                                }
                            });
                        });
                }

                // Inline New Folder creation bar
                if let Some(nf) = &mut self.new_folder {
                    let mut make = false;
                    let mut cancel = false;
                    egui::Frame::new()
                        .fill(BG_PANEL)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("New folder:").size(12.0).color(TEXT_MUTED));
                                let r = ui.add(egui::TextEdit::singleline(nf).desired_width(200.0));
                                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    make = true;
                                }
                                if ui.add(egui::Button::new(RichText::new("Create").size(12.0).color(Color32::WHITE)).fill(ACCENT)).clicked() {
                                    make = true;
                                }
                                if ui.add(egui::Button::new(RichText::new("Cancel").size(12.0)).fill(BG_TILE)).clicked() {
                                    cancel = true;
                                }
                            });
                        });
                    if make && !nf.trim().is_empty() {
                        let dir = self.listing.dir.join(safe_name(nf.trim()));
                        match std::fs::create_dir_all(&dir) {
                            Ok(()) => {
                                self.new_folder = None;
                                nav_dir = Some(dir);
                            }
                            Err(e) => self.error = Some(format!("{}: {e}", dir.display())),
                        }
                    } else if cancel {
                        self.new_folder = None;
                    }
                }

                // Subtle waiting indicator if System Picker is currently copying files
                if self.waiting_system_picker {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Waiting for Android System Picker…").size(11.5).color(ACCENT));
                        if ui.small_button("Dismiss").clicked() {
                            self.waiting_system_picker = false;
                        }
                    });
                }

                // ==================== 2. MAIN PANES ====================
                let footer_reserve = if self.error.is_some() { 64.0 } else { 44.0 };
                let body_h = (ui.available_height() - footer_reserve).max(110.0);
                let body_w = ui.available_width();
                let sidebar_w = if body_w >= 460.0 { 126.0 } else { 104.0 };

                let preview_path = self.active_preview_path().filter(|p| p.is_file());
                let show_preview = body_w >= 560.0 && preview_path.is_some();
                let preview_w = if show_preview { 172.0 } else { 0.0 };
                let gap = 6.0;
                let browser_w = (body_w - sidebar_w - gap - if show_preview { preview_w + gap } else { 0.0 }).max(140.0);

                ui.allocate_ui_with_layout(
                    Vec2::new(body_w, body_h),
                    egui::Layout::left_to_right(egui::Align::TOP),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = gap;

                        // ---- LEFT SIDEBAR: Quick Places ----
                        egui::Frame::new()
                            .fill(BG_PANEL)
                            .stroke(Stroke::new(1.0, BORDER))
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::same(6))
                            .show(ui, |ui| {
                                ui.set_width(sidebar_w - 12.0);
                                ui.set_height(body_h - 12.0);
                                ui.label(RichText::new("PLACES").size(10.0).strong().color(TEXT_MUTED));
                                ui.add_space(2.0);
                                egui::ScrollArea::vertical()
                                    .id_salt("android-picker-sidebar")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        // Find most specific matching root so "Imported" highlights instead of "Rustudio".
                                        let active_idx = roots
                                            .iter()
                                            .enumerate()
                                            .filter(|(_, r)| self.listing.dir.starts_with(&r.path))
                                            .max_by_key(|(_, r)| r.path.as_os_str().len())
                                            .map(|(i, _)| i);
                                        for (idx, r) in roots.iter().enumerate() {
                                            let active = active_idx == Some(idx);
                                            let row_w = ui.available_width();
                                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(row_w, 30.0), Sense::click());
                                            if active {
                                                ui.painter().rect_filled(rect, 5.0, BG_SELECTED);
                                                let bar = Rect::from_min_size(
                                                    Pos2::new(rect.left() + 1.0, rect.top() + 5.0),
                                                    Vec2::new(3.0, rect.height() - 10.0),
                                                );
                                                ui.painter().rect_filled(bar, 1.5, ACCENT);
                                            } else if resp.hovered() {
                                                ui.painter().rect_filled(rect, 5.0, BG_HOVER);
                                            }
                                            ui.painter().with_clip_rect(rect.shrink2(Vec2::new(8.0, 2.0))).text(
                                                Pos2::new(rect.left() + 10.0, rect.center().y),
                                                Align2::LEFT_CENTER,
                                                r.label,
                                                FontId::proportional(12.5),
                                                if active { Color32::WHITE } else { TEXT_PRIMARY },
                                            );
                                            if resp.clicked() {
                                                nav_dir = Some(r.path.clone());
                                            }
                                        }
                                    });
                            });

                        // ---- CENTER FILE BROWSER ----
                        egui::Frame::new()
                            .fill(BG_BROWSER)
                            .stroke(Stroke::new(1.0, BORDER))
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::same(6))
                            .show(ui, |ui| {
                                ui.set_width(browser_w - 12.0);
                                ui.set_height(body_h - 12.0);
                                egui::ScrollArea::vertical()
                                    .id_salt("android-picker-browser")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        if self.listing.folders.is_empty() && self.listing.files.is_empty() {
                                            ui.add_space(18.0);
                                            ui.vertical_centered(|ui| {
                                                let msg = if self.listing.hidden_by_filter > 0 {
                                                    format!(
                                                        "No matching files here ({} hidden by filter).\nTap \"All files\" below to show everything.",
                                                        self.listing.hidden_by_filter
                                                    )
                                                } else {
                                                    "Empty folder.\nUse System Picker or copy files into this folder.".to_string()
                                                };
                                                ui.label(RichText::new(msg).size(12.5).color(TEXT_MUTED));
                                            });
                                        } else {
                                            match self.view_mode {
                                                ViewMode::List => {
                                                    self.render_list_view(ui, ctx, &mut nav_dir, &mut result);
                                                }
                                                ViewMode::Grid => {
                                                    self.render_grid_view(ui, ctx, &mut nav_dir, &mut result);
                                                }
                                            }
                                        }
                                    });
                            });

                        // ---- RIGHT PREVIEW PANEL ----
                        if show_preview {
                            if let Some(ref p) = preview_path {
                                self.render_preview_panel(ui, ctx, p, preview_w, body_h, &mut result);
                            }
                        }
                    },
                );

                // Error line if any
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, RichText::new(e).size(11.5));
                }

                // ==================== 3. BOTTOM FOOTER ====================
                ui.horizontal(|ui| {
                    // Left side: Save filename input or selection status + filter toggle
                    if self.kind == Kind::SaveFile {
                        ui.label(RichText::new("Name:").size(12.5).color(TEXT_MUTED));
                        let name_w = (ui.available_width() - 250.0).clamp(110.0, 260.0);
                        ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(name_w));
                    } else if self.multi {
                        if ui
                            .add_enabled(
                                !self.listing.files.is_empty(),
                                egui::Button::new(RichText::new("Select All").size(12.0)).fill(BG_TILE),
                            )
                            .clicked()
                        {
                            for f in &self.listing.files {
                                if !self.selected_multi.iter().any(|p| p == &f.path) {
                                    self.selected_multi.push(f.path.clone());
                                }
                            }
                        }
                        if ui
                            .add_enabled(
                                !self.selected_multi.is_empty(),
                                egui::Button::new(RichText::new("Clear").size(12.0)).fill(BG_TILE),
                            )
                            .clicked()
                        {
                            self.selected_multi.clear();
                            self.selected = None;
                        }
                    } else if let Some(sel) = &self.selected {
                        let fname = sel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        ui.add(
                            egui::Label::new(RichText::new(fname).size(12.5).color(TEXT_PRIMARY)).truncate(),
                        );
                    }

                    // Filter / "All files" one-tap toggle
                    if !self.request.filters.is_empty() {
                        let all_on = self.show_all_files;
                        let filter_label = if all_on {
                            "All files".to_string()
                        } else {
                            let summary = filter_summary(&self.request);
                            if self.listing.hidden_by_filter > 0 {
                                format!("{summary} (+{} hidden)", self.listing.hidden_by_filter)
                            } else {
                                summary
                            }
                        };
                        let btn = egui::Button::new(
                            RichText::new(filter_label)
                                .size(11.5)
                                .color(if all_on { Color32::WHITE } else { TEXT_MUTED }),
                        )
                        .fill(if all_on { ACCENT_SOFT } else { BG_TILE })
                        .stroke(Stroke::new(1.0, if all_on { ACCENT } else { BORDER }))
                        .min_size(Vec2::new(0.0, 26.0));
                        if ui
                            .add(btn)
                            .on_hover_text("Tap to toggle between filtered extensions and All files")
                            .clicked()
                        {
                            self.show_all_files = !self.show_all_files;
                            self.refresh_listing();
                        }
                    } else {
                        ui.label(RichText::new("All files").size(11.5).color(TEXT_MUTED));
                    }

                    // Right side: Cancel + primary Open/Save/Import action
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.multi {
                            let count = self.selected_multi.len();
                            let label = format!("Import ({count})");
                            if ui
                                .add_enabled(
                                    count > 0,
                                    egui::Button::new(RichText::new(label).size(12.5).strong().color(Color32::WHITE))
                                        .fill(if count > 0 { ACCENT } else { BG_TILE })
                                        .min_size(Vec2::new(92.0, 28.0)),
                                )
                                .clicked()
                            {
                                result = Some(Some(self.selected_multi.clone()));
                            }
                        } else {
                            let (verb, ready) = match self.kind {
                                Kind::PickFile => ("Open", self.selected.is_some()),
                                Kind::PickFolder => ("Choose", true),
                                Kind::SaveFile => ("Save", !self.name.trim().is_empty()),
                            };
                            if ui
                                .add_enabled(
                                    ready,
                                    egui::Button::new(RichText::new(verb).size(12.5).strong().color(Color32::WHITE))
                                        .fill(if ready { ACCENT } else { BG_TILE })
                                        .min_size(Vec2::new(76.0, 28.0)),
                                )
                                .clicked()
                            {
                                result = Some(self.confirm_single());
                            }
                        }
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Cancel").size(12.5).color(TEXT_PRIMARY))
                                    .fill(BG_TILE)
                                    .min_size(Vec2::new(68.0, 28.0)),
                            )
                            .clicked()
                        {
                            result = Some(None);
                        }
                    });
                });
            });

        if let Some(dir) = nav_dir {
            self.go(&dir);
        }
        if !open {
            result = Some(None);
        }
        if result.is_some() {
            self.clear_thumbnails();
        }
        result
    }

    fn confirm_single(&self) -> Option<Vec<PathBuf>> {
        match self.kind {
            Kind::PickFile => self.selected.clone().map(|p| vec![p]),
            Kind::PickFolder => Some(vec![self.selected.clone().unwrap_or_else(|| self.listing.dir.clone())]),
            Kind::SaveFile => Some(vec![self.listing.dir.join(self.save_name())]),
        }
    }

    fn render_list_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        nav_dir: &mut Option<PathBuf>,
        result: &mut Option<Option<Vec<PathBuf>>>,
    ) {
        let row_w = ui.available_width();
        let row_h = 38.0;

        // Folders first (left-aligned)
        for idx in 0..self.listing.folders.len() {
            let path = self.listing.folders[idx].path.clone();
            let name = self.listing.folders[idx].name.clone();
            let sel = self.kind == Kind::PickFolder && self.selected.as_deref() == Some(path.as_path());
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(row_w, row_h), Sense::click());
            if ui.is_rect_visible(rect) {
                if sel {
                    ui.painter().rect_filled(rect, 5.0, BG_SELECTED);
                    ui.painter().rect_stroke(rect, 5.0, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 5.0, BG_HOVER);
                }
                let icon_rect = Rect::from_min_size(Pos2::new(rect.left() + 10.0, rect.center().y - 8.0), Vec2::new(20.0, 16.0));
                paint_folder_icon(ui.painter(), icon_rect);

                let text_clip = Rect::from_min_max(
                    Pos2::new(rect.left() + 38.0, rect.top()),
                    Pos2::new((rect.right() - 24.0).max(rect.left() + 48.0), rect.bottom()),
                );
                ui.painter().with_clip_rect(text_clip).text(
                    Pos2::new(text_clip.left(), rect.center().y),
                    Align2::LEFT_CENTER,
                    &name,
                    FontId::proportional(13.0),
                    TEXT_PRIMARY,
                );
                ui.painter().text(
                    Pos2::new(rect.right() - 12.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    "›",
                    FontId::proportional(15.0),
                    TEXT_MUTED,
                );
            }
            if resp.double_clicked() || (resp.clicked() && self.kind != Kind::PickFolder) {
                *nav_dir = Some(path);
            } else if resp.clicked() {
                self.selected = Some(path);
            }
        }

        // Files (left-aligned name, thumbnail or typed badge, right-aligned size)
        let mut to_request_thumbs: Vec<PathBuf> = Vec::new();
        for idx in 0..self.listing.files.len() {
            let path = self.listing.files[idx].path.clone();
            let name = self.listing.files[idx].name.clone();
            let size = self.listing.files[idx].size;
            let badge = self.listing.files[idx].badge;

            let is_checked = self.multi && self.selected_multi.iter().any(|p| p == &path);
            let is_selected = if self.multi {
                is_checked || self.selected.as_deref() == Some(path.as_path())
            } else {
                self.selected.as_deref() == Some(path.as_path())
            };

            let (rect, resp) = ui.allocate_exact_size(Vec2::new(row_w, row_h), Sense::click());
            if ui.is_rect_visible(rect) {
                if badge.is_image() {
                    to_request_thumbs.push(path.clone());
                }
                if is_selected {
                    ui.painter().rect_filled(rect, 5.0, BG_SELECTED);
                    ui.painter().rect_stroke(rect, 5.0, Stroke::new(1.0, ACCENT), StrokeKind::Inside);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 5.0, BG_HOVER);
                }

                let mut x = rect.left() + 8.0;

                // Checkbox in multi-select mode
                if self.multi {
                    let cb = Rect::from_min_size(Pos2::new(x, rect.center().y - 8.0), Vec2::splat(16.0));
                    ui.painter().rect_filled(cb, 3.0, if is_checked { ACCENT } else { BG_PANEL });
                    ui.painter().rect_stroke(
                        cb,
                        3.0,
                        Stroke::new(1.0, if is_checked { ACCENT } else { BORDER }),
                        StrokeKind::Inside,
                    );
                    if is_checked {
                        ui.painter().text(cb.center(), Align2::CENTER_CENTER, "✓", FontId::proportional(11.0), Color32::WHITE);
                    }
                    x += 24.0;
                }

                // Thumbnail (28x28) or typed file badge
                let icon_box = Rect::from_min_size(Pos2::new(x, rect.center().y - 14.0), Vec2::new(34.0, 28.0));
                if let Some(ThumbEntry::Loaded { tex, .. }) = self.thumbs.get(&path) {
                    let fit = fit_aspect(icon_box, tex.size_vec2());
                    egui::Image::new(tex).corner_radius(3.0).paint_at(ui, fit);
                } else {
                    let pill = Rect::from_center_size(icon_box.center(), Vec2::new(34.0, 20.0));
                    paint_badge(ui.painter(), pill, badge, 9.5);
                }
                x += 42.0;

                // Right-aligned human-readable file size
                let size_str = format_size(size);
                let size_reserve = 68.0;
                ui.painter().text(
                    Pos2::new(rect.right() - 10.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    size_str,
                    FontId::proportional(11.5),
                    TEXT_MUTED,
                );

                // Left-aligned filename clipped cleanly before size column
                let text_clip = Rect::from_min_max(
                    Pos2::new(x, rect.top()),
                    Pos2::new((rect.right() - size_reserve).max(x + 24.0), rect.bottom()),
                );
                ui.painter().with_clip_rect(text_clip).text(
                    Pos2::new(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &name,
                    FontId::proportional(13.0),
                    if is_selected { Color32::WHITE } else { TEXT_PRIMARY },
                );
            }

            if resp.clicked() {
                if self.multi {
                    if let Some(pos) = self.selected_multi.iter().position(|p| p == &path) {
                        self.selected_multi.remove(pos);
                    } else {
                        self.selected_multi.push(path.clone());
                    }
                    self.selected = Some(path.clone());
                } else {
                    self.selected = Some(path.clone());
                    if self.kind == Kind::SaveFile {
                        self.name = name.clone();
                    }
                }
            }
            if resp.double_clicked() && self.kind == Kind::PickFile && !self.multi {
                *result = Some(Some(vec![path]));
            }
        }

        for p in to_request_thumbs {
            self.request_thumb_if_needed(ctx, &p);
        }
    }

    fn render_grid_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        nav_dir: &mut Option<PathBuf>,
        result: &mut Option<Option<Vec<PathBuf>>>,
    ) {
        let avail_w = ui.available_width();
        let gap = 8.0;
        let min_tile_w = 96.0;
        let cols = ((avail_w + gap) / (min_tile_w + gap)).floor().max(2.0) as usize;
        let tile_w = ((avail_w - gap * (cols as f32 - 1.0)) / cols as f32).floor().max(80.0);
        let tile_h = 112.0;

        enum GridItem {
            Folder(usize),
            File(usize),
        }
        let mut items = Vec::with_capacity(self.listing.folders.len() + self.listing.files.len());
        for i in 0..self.listing.folders.len() {
            items.push(GridItem::Folder(i));
        }
        for i in 0..self.listing.files.len() {
            items.push(GridItem::File(i));
        }

        let mut to_request_thumbs: Vec<PathBuf> = Vec::new();

        for row in items.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for item in row {
                    match *item {
                        GridItem::Folder(idx) => {
                            let path = self.listing.folders[idx].path.clone();
                            let name = self.listing.folders[idx].name.clone();
                            let sel = self.kind == Kind::PickFolder && self.selected.as_deref() == Some(path.as_path());
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());
                            if ui.is_rect_visible(rect) {
                                let bg = if sel { BG_SELECTED } else if resp.hovered() { BG_HOVER } else { BG_TILE };
                                ui.painter().rect_filled(rect, 7.0, bg);
                                if sel {
                                    ui.painter().rect_stroke(rect, 7.0, Stroke::new(1.5, ACCENT), StrokeKind::Inside);
                                }
                                let icon_r = Rect::from_center_size(
                                    Pos2::new(rect.center().x, rect.top() + 38.0),
                                    Vec2::new(42.0, 32.0),
                                );
                                paint_folder_icon(ui.painter(), icon_r);

                                let name_clip = Rect::from_min_max(
                                    Pos2::new(rect.left() + 6.0, rect.top() + 68.0),
                                    Pos2::new(rect.right() - 6.0, rect.bottom() - 6.0),
                                );
                                ui.painter().with_clip_rect(name_clip).text(
                                    Pos2::new(rect.center().x, rect.top() + 84.0),
                                    Align2::CENTER_CENTER,
                                    truncate_middle(&name, 14),
                                    FontId::proportional(11.5),
                                    TEXT_PRIMARY,
                                );
                            }
                            if resp.double_clicked() || (resp.clicked() && self.kind != Kind::PickFolder) {
                                *nav_dir = Some(path);
                            } else if resp.clicked() {
                                self.selected = Some(path);
                            }
                        }
                        GridItem::File(idx) => {
                            let path = self.listing.files[idx].path.clone();
                            let name = self.listing.files[idx].name.clone();
                            let size = self.listing.files[idx].size;
                            let badge = self.listing.files[idx].badge;

                            let is_checked = self.multi && self.selected_multi.iter().any(|p| p == &path);
                            let is_selected = if self.multi {
                                is_checked || self.selected.as_deref() == Some(path.as_path())
                            } else {
                                self.selected.as_deref() == Some(path.as_path())
                            };

                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());
                            if ui.is_rect_visible(rect) {
                                if badge.is_image() {
                                    to_request_thumbs.push(path.clone());
                                }
                                let bg = if is_selected { BG_SELECTED } else if resp.hovered() { BG_HOVER } else { BG_TILE };
                                ui.painter().rect_filled(rect, 7.0, bg);
                                if is_selected {
                                    ui.painter().rect_stroke(rect, 7.0, Stroke::new(1.5, ACCENT), StrokeKind::Inside);
                                }

                                let thumb_box = Rect::from_min_max(
                                    Pos2::new(rect.left() + 6.0, rect.top() + 6.0),
                                    Pos2::new(rect.right() - 6.0, rect.top() + 72.0),
                                );
                                ui.painter().rect_filled(thumb_box, 5.0, BG_BROWSER);
                                if let Some(ThumbEntry::Loaded { tex, .. }) = self.thumbs.get(&path) {
                                    let fit = fit_aspect(thumb_box.shrink(2.0), tex.size_vec2());
                                    egui::Image::new(tex).corner_radius(4.0).paint_at(ui, fit);
                                } else {
                                    let pill = Rect::from_center_size(thumb_box.center(), Vec2::new(42.0, 22.0));
                                    paint_badge(ui.painter(), pill, badge, 11.0);
                                }

                                if is_checked {
                                    let check_r = Rect::from_min_size(
                                        Pos2::new(rect.right() - 22.0, rect.top() + 6.0),
                                        Vec2::splat(16.0),
                                    );
                                    ui.painter().rect_filled(check_r, 8.0, ACCENT);
                                    ui.painter().text(
                                        check_r.center(),
                                        Align2::CENTER_CENTER,
                                        "✓",
                                        FontId::proportional(10.5),
                                        Color32::WHITE,
                                    );
                                }

                                let label_clip = Rect::from_min_max(
                                    Pos2::new(rect.left() + 6.0, rect.top() + 75.0),
                                    Pos2::new(rect.right() - 6.0, rect.top() + 94.0),
                                );
                                ui.painter().with_clip_rect(label_clip).text(
                                    Pos2::new(label_clip.left(), label_clip.center().y),
                                    Align2::LEFT_CENTER,
                                    truncate_middle(&name, 14),
                                    FontId::proportional(11.5),
                                    if is_selected { Color32::WHITE } else { TEXT_PRIMARY },
                                );
                                ui.painter().text(
                                    Pos2::new(rect.left() + 6.0, rect.bottom() - 10.0),
                                    Align2::LEFT_CENTER,
                                    format_size(size),
                                    FontId::proportional(10.0),
                                    TEXT_MUTED,
                                );
                            }

                            if resp.clicked() {
                                if self.multi {
                                    if let Some(pos) = self.selected_multi.iter().position(|p| p == &path) {
                                        self.selected_multi.remove(pos);
                                    } else {
                                        self.selected_multi.push(path.clone());
                                    }
                                    self.selected = Some(path.clone());
                                } else {
                                    self.selected = Some(path.clone());
                                    if self.kind == Kind::SaveFile {
                                        self.name = name.clone();
                                    }
                                }
                            }
                            if resp.double_clicked() && self.kind == Kind::PickFile && !self.multi {
                                *result = Some(Some(vec![path]));
                            }
                        }
                    }
                }
            });
            ui.add_space(2.0);
        }

        for p in to_request_thumbs {
            self.request_thumb_if_needed(ctx, &p);
        }
    }

    fn render_preview_panel(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        path: &Path,
        preview_w: f32,
        body_h: f32,
        result: &mut Option<Option<Vec<PathBuf>>>,
    ) {
        let badge = FileBadge::from_path(path);
        if badge.is_image() {
            self.request_preview_if_needed(ctx, path);
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let file_size = self
            .listing
            .files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.size)
            .unwrap_or_else(|| std::fs::metadata(path).map(|m| m.len()).unwrap_or(0));

        egui::Frame::new()
            .fill(BG_PANEL)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(7))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                let inner_w = preview_w - 16.0;
                ui.set_width(inner_w);
                ui.set_height(body_h - 16.0);

                ui.label(RichText::new("PREVIEW").size(10.0).strong().color(TEXT_MUTED));
                ui.add_space(4.0);

                // Preview box (up to 150x150)
                let box_side = inner_w.min(150.0).min((body_h - 150.0).max(80.0));
                let (pbox, _) = ui.allocate_exact_size(Vec2::new(inner_w, box_side), Sense::hover());
                ui.painter().rect_filled(pbox, 6.0, BG_BROWSER);
                ui.painter().rect_stroke(pbox, 6.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);

                let mut dims: Option<(u32, u32)> = None;
                if badge.is_image() {
                    if let Some(pt) = self.preview_tex.as_ref().filter(|pt| pt.path == path) {
                        dims = pt.dims;
                        if let Some(tex) = &pt.tex {
                            let fit = fit_aspect(pbox.shrink(4.0), tex.size_vec2());
                            egui::Image::new(tex).corner_radius(4.0).paint_at(ui, fit);
                        } else {
                            let pill = Rect::from_center_size(pbox.center(), Vec2::new(52.0, 26.0));
                            paint_badge(ui.painter(), pill, badge, 12.0);
                        }
                    } else if let Some(ThumbEntry::Loaded { tex, dims: td }) = self.thumbs.get(path) {
                        dims = Some(*td);
                        let fit = fit_aspect(pbox.shrink(4.0), tex.size_vec2());
                        egui::Image::new(tex).corner_radius(4.0).paint_at(ui, fit);
                    } else {
                        ui.painter().text(
                            pbox.center(),
                            Align2::CENTER_CENTER,
                            "Loading…",
                            FontId::proportional(11.5),
                            TEXT_MUTED,
                        );
                    }
                } else {
                    let pill = Rect::from_center_size(pbox.center(), Vec2::new(56.0, 28.0));
                    paint_badge(ui.painter(), pill, badge, 13.0);
                }

                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(&name).size(12.5).strong().color(TEXT_PRIMARY)).wrap());
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    let (br, _) = ui.allocate_exact_size(Vec2::new(38.0, 18.0), Sense::hover());
                    paint_badge(ui.painter(), br, badge, 9.5);
                    ui.label(RichText::new(format_size(file_size)).size(11.5).color(TEXT_MUTED));
                });

                if let Some((w, h)) = dims {
                    ui.label(RichText::new(format!("{w} × {h} px")).size(11.5).color(TEXT_MUTED));
                }

                // Quick primary action button inside preview pane
                if !self.multi && self.kind == Kind::PickFile && ui.available_height() >= 34.0 {
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Open File").size(12.0).strong().color(Color32::WHITE))
                                    .fill(ACCENT)
                                    .min_size(Vec2::new(inner_w, 28.0)),
                            )
                            .clicked()
                        {
                            *result = Some(Some(vec![path.to_path_buf()]));
                        }
                    });
                }
            });
    }

    /// The name to save as, with the first filter's extension when none was
    /// typed (as desktop dialogs add it).
    fn save_name(&self) -> String {
        let name = safe_name(self.name.trim());
        let has_ext = Path::new(&name).extension().is_some();
        match (has_ext, self.request.filters.first().and_then(|(_, e)| e.first())) {
            (false, Some(ext)) => format!("{name}.{ext}"),
            _ => name,
        }
    }
}

// ------------------------------------------------------------ UI Helpers

fn paint_folder_icon(painter: &egui::Painter, rect: Rect) {
    let tab_h = (rect.height() * 0.28).max(3.0);
    let tab = Rect::from_min_size(rect.min, Vec2::new(rect.width() * 0.44, tab_h + 2.0));
    painter.rect_filled(tab, CornerRadius::same(2), FOLDER_TAB);
    let body = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + tab_h), rect.max);
    painter.rect_filled(body, CornerRadius::same(2), FOLDER_BODY);
}

fn paint_badge(painter: &egui::Painter, rect: Rect, badge: FileBadge, font_size: f32) {
    painter.rect_filled(rect, 4.0, badge.bg());
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, badge.color().gamma_multiply(0.65)), StrokeKind::Inside);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        badge.label(),
        FontId::proportional(font_size),
        badge.color(),
    );
}

fn fit_aspect(bounds: Rect, img_size: Vec2) -> Rect {
    if img_size.x <= 0.0 || img_size.y <= 0.0 {
        return bounds;
    }
    let scale = (bounds.width() / img_size.x).min(bounds.height() / img_size.y);
    let size = img_size * scale;
    Rect::from_center_size(bounds.center(), size)
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.0} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.2} GB", b / GB)
    }
}

fn truncate_middle(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars || max_chars < 5 {
        return s.to_string();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - 1 - head;
    let mut out = String::with_capacity(s.len());
    for &c in &chars[..head] {
        out.push(c);
    }
    out.push('…');
    for &c in &chars[chars.len() - tail..] {
        out.push(c);
    }
    out
}

fn filter_summary(request: &Request) -> String {
    let mut exts: Vec<String> = Vec::new();
    for (_, list) in &request.filters {
        for e in list {
            let tag = format!("*.{}", e.trim_start_matches('.').to_ascii_lowercase());
            if !exts.contains(&tag) {
                exts.push(tag);
            }
        }
    }
    if exts.is_empty() {
        "All files".to_string()
    } else if exts.len() <= 3 {
        exts.join(", ")
    } else {
        format!("{}, {} +{}", exts[0], exts[1], exts.len() - 2)
    }
}

/// A file name without path separators or control characters.
fn safe_name(s: &str) -> String {
    let out: String = s.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    if out.is_empty() { "untitled".into() } else { out }
}

/// A path with the app's folder prefix replaced by its label.
fn shorten(p: &Path) -> String {
    let all = roots(true);
    if let Some(best) = all
        .iter()
        .filter(|r| p.starts_with(&r.path))
        .max_by_key(|r| r.path.as_os_str().len())
    {
        if let Ok(rest) = p.strip_prefix(&best.path) {
            let rest = rest.display().to_string();
            return if rest.is_empty() {
                best.label.to_string()
            } else {
                format!("{} / {}", best.label, rest.replace('/', " / "))
            };
        }
    }
    p.display().to_string()
}

/// The place or model this activity was started to open ("Open with
/// rustudio" in a file manager, a share sheet, a download notification):
/// the Intent's data URI, copied into the app's folder when it is a
/// `content:` URI (another app's document, readable only through its
/// ContentResolver) — unless the last start ended while opening that same
/// file ([`skip_crashed`]).
pub fn launch_file(_app: &super::AndroidApp) -> Option<PathBuf> {
    let uri = match super::jni::launch_intent_uri() {
        Ok(u) => u?,
        Err(e) => {
            log::warn!("launch intent: {e}");
            return None;
        }
    };
    log::info!("launch intent: {uri}");
    let path = import_uri(&uri)?;
    if skip_crashed(&path) {
        return None;
    }
    opening_marker(Some(&path));
    Some(path)
}

/// A local path for `uri`, through the running activity.
pub fn import_uri(uri: &str) -> Option<PathBuf> {
    import_uri_with(uri, &|f| super::jni::with_activity(f))
}

/// [`import_uri`] through the Activity of a raw `ANativeActivity` (a
/// second activity of the process: src/android/intent.rs).
///
/// # Safety
/// `activity` must be the live `ANativeActivity`.
pub unsafe fn import_uri_from(activity: *mut ndk_sys::ANativeActivity, uri: &str) -> Option<PathBuf> {
    // SAFETY: the caller's live struct.
    import_uri_with(uri, &|f| unsafe { super::jni::with_native_activity(activity, f) })
}

/// A JNI call made with some Activity: the name and the bytes of a URI.
type Fetch<'a> = &'a mut dyn FnMut(&mut jni::Env, &jni::objects::JObject) -> jni::errors::Result<(Option<String>, Vec<u8>)>;

fn import_uri_with(uri: &str, with: &dyn Fn(Fetch<'_>) -> super::jni::Result<(Option<String>, Vec<u8>)>) -> Option<PathBuf> {
    if let Some(rest) = uri.strip_prefix("file://") {
        let p = PathBuf::from(percent_decode(rest));
        return p.is_file().then_some(p);
    }
    if !uri.starts_with("content://") {
        return None;
    }
    let (name, bytes) = match with(&mut |env, activity| {
        // (a provider without a name column throws: the name is optional,
        // the exception must not stand in the way of the read)
        let name = super::jni::display_name(env, activity, uri).unwrap_or_else(|e| {
            log::warn!("{uri}: no display name: {e}");
            env.exception_clear();
            None
        });
        let bytes = super::jni::content_bytes(env, activity, uri)?;
        Ok((name, bytes))
    }) {
        Ok(x) => x,
        Err(e) => {
            log::error!("{uri}: {e}");
            return None;
        }
    };
    let name = name.filter(|n| !n.is_empty()).unwrap_or_else(|| "place.rbxl".to_string());
    let dir = super::data_dir().join("Imported");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("{}: {e}", dir.display());
        return None;
    }
    // The same file handed over again (the activity recreated with its
    // VIEW intent, "Open with" twice) is the copy already made, not
    // "name (2)": the copies are compared byte for byte.
    let wanted = dir.join(safe_name(&name));
    if let Some(same) = same_import(&wanted, &bytes) {
        log::info!("{name} is already imported as {}", same.display());
        return Some(same);
    }
    let path = unique_path(&wanted);
    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            log::info!("imported {} bytes to {}", bytes.len(), path.display());
            Some(path)
        }
        Err(e) => {
            log::error!("{}: {e}", path.display());
            None
        }
    }
}

/// Among `p` and its `p (n)` copies, the one holding exactly `bytes`.
fn same_import(p: &Path, bytes: &[u8]) -> Option<PathBuf> {
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = p.parent().unwrap_or(Path::new("."));
    std::iter::once(p.to_path_buf())
        .chain((2..100).map(|n| dir.join(format!("{stem} ({n}){ext}"))))
        .take_while(|c| c.exists())
        .find(|c| std::fs::metadata(c).is_ok_and(|m| m.len() == bytes.len() as u64) && std::fs::read(c).is_ok_and(|b| b == bytes))
}

// ------------------------------------------------------ opened while running

/// Files handed over while the editor runs (a second activity's VIEW
/// intent, src/android/intent.rs), for the editor's next frame.
static OPENED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

pub fn push_opened(p: PathBuf) {
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).push(p);
}

/// The files pushed since the last call (the editor opens them).
pub fn take_opened() -> Vec<PathBuf> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Raw JNI bridge called from `crates/rustudio-android/src/lib.rs` on SAF open-document result.
///
/// Receives either newline-separated local file paths copied into `cacheDir/picked/` by
/// `RustudioActivity.java`, raw `content://` URIs (fallback), or an empty string when cancelled.
/// Delivers directly to [`SAF_PICKER_RESULT`] for the waiting [`Picker`] — never to [`OPENED`].
///
/// # Safety
/// Must be called by the JVM with valid `JNIEnv*` and optional `jstring`.
pub unsafe fn jni_on_saf_document_opened(
    env: *mut std::ffi::c_void,
    uri: *mut std::ffi::c_void,
) {
    if env.is_null() {
        return;
    }
    let s = if uri.is_null() {
        String::new()
    } else {
        let env_ptr = env as *mut jni::sys::JNIEnv;
        let jstr = uri as jni::sys::jstring;
        unsafe {
            let Some(vtable) = (*env_ptr).as_ref() else {
                return;
            };
            let len = (vtable.v1_1.GetStringLength)(env_ptr, jstr);
            if len <= 0 {
                String::new()
            } else {
                let ptr = (vtable.v1_1.GetStringChars)(env_ptr, jstr, std::ptr::null_mut());
                if ptr.is_null() {
                    String::new()
                } else {
                    let slice = std::slice::from_raw_parts(ptr, len as usize);
                    let decoded = String::from_utf16_lossy(slice);
                    (vtable.v1_1.ReleaseStringChars)(env_ptr, jstr, ptr);
                    decoded
                }
            }
        }
    };

    let mut paths = Vec::new();
    for line in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("content://") || line.starts_with("file://") {
            if let Some(p) = import_uri(line) {
                paths.push(p);
            }
        } else {
            let p = PathBuf::from(line);
            if p.is_file() {
                paths.push(p);
            }
        }
    }
    set_saf_picker_result(paths);
    super::lifecycle::wake();
}

// ------------------------------------------------------- the opening marker

/// Lines for Output at start (what [`launch_file`] decided).
static NOTICES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn take_notices() -> Vec<String> {
    std::mem::take(&mut *NOTICES.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The file an open is under way for, until the start has survived
/// [`SETTLED`] past it.
static OPENING: std::sync::Mutex<Option<(PathBuf, Instant)>> = std::sync::Mutex::new(None);
/// A start that ran this long after opening a file counts as having
/// opened it.
const SETTLED: std::time::Duration = std::time::Duration::from_secs(20);

/// `state/opening`: the file the last automatic open was of, left behind
/// when the process ended within [`SETTLED`] of it.
fn marker_path() -> PathBuf {
    super::state_dir().join("opening")
}

fn opening_marker(p: Option<&Path>) {
    let m = marker_path();
    match p {
        Some(p) => {
            let _ = std::fs::create_dir_all(super::state_dir());
            if let Err(e) = std::fs::write(&m, p.to_string_lossy().as_bytes()) {
                log::warn!("{}: {e}", m.display());
            }
            opening(p);
        }
        None => {
            let _ = std::fs::remove_file(&m);
        }
    }
}

/// Whether the last start ended (a crash, a kill) within [`SETTLED`] of
/// opening `p` by itself.
fn skip_crashed(p: &Path) -> bool {
    let Ok(last) = std::fs::read_to_string(marker_path()) else { return false };
    if Path::new(last.trim()) != p {
        return false;
    }
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let line = format!(
        "rustudio ended within {} s of opening {name} the last time, so it wasn't opened again by itself. \
         File ▸ Open Recent opens it on purpose (adb logcat -s rustudio RustStdoutStderr tells what ended the last start).",
        SETTLED.as_secs()
    );
    log::warn!("{line}");
    NOTICES.lock().unwrap_or_else(|e| e.into_inner()).push(line);
    true
}

/// A file is being opened (the editor's `open_path`): the clock for
/// [`settle`] starts (again).
pub fn opening(p: &Path) {
    *OPENING.lock().unwrap_or_else(|e| e.into_inner()) = Some((p.to_path_buf(), Instant::now()));
}

/// Once a frame: a start that has run [`SETTLED`] past its last open has
/// opened that file for good; the marker it may have left goes.
pub fn settle() {
    let mut o = OPENING.lock().unwrap_or_else(|e| e.into_inner());
    let Some((p, since)) = o.as_ref() else { return };
    if since.elapsed() < SETTLED {
        return;
    }
    if std::fs::read_to_string(marker_path()).is_ok_and(|last| Path::new(last.trim()) == p) {
        opening_marker(None);
    }
    *o = None;
}

/// `name.ext`, else `name (2).ext`, … so an import never overwrites.
fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = p.parent().unwrap_or(Path::new("."));
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or_else(|| p.to_path_buf())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
