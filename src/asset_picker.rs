//! Godot/Roblox Studio-style visual Asset Picker / Upload-to-Roblox / ID Browser dialog.
//!
//! Opened whenever the user clicks a `Variant::ContentId` or `Variant::Content`
//! property in the Properties panel (such as `Image`, `Texture`, `TextureID`,
//! `HoverImage`, `PressedImage`, `SoundId`, `AudioContent`, `MeshId`,
//! `MeshContent`, `AnimationId`, `ShirtTemplate`, `PantsTemplate`, `Skybox*`).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use rbx_dom_weak::types::Ref;
use rustudio_account::cloud::{CloudApi, Endpoints};
use rustudio_account::session::Session;

#[cfg(target_os = "android")]
use crate::file_dialog::Kind as FileDialogKind;
use crate::file_dialog::Request as FileDialogRequest;
use crate::toolbox::api::{self as tb_api, AnonHttp, Failure, Filters, Http, ListReq, SessionHttp, Source};
use crate::toolbox::categories::Kind as ToolboxKind;
use crate::toolbox::model as tb_model;

/// Maximum number of `egui::TextureHandle` entries kept in the Asset Picker RAM cache.
const THUMB_CACHE_CAP: usize = 48;
/// Maximum decoded dimension (width/height) for remote Roblox asset thumbnails.
const THUMB_MAX_DIM: u32 = 128;
/// Maximum decoded dimension (width/height) for local upload image previews.
const LOCAL_PREVIEW_MAX_DIM: u32 = 256;
/// Requested thumbnail size from Roblox's batch thumbnails API.
const THUMB_FETCH_SIZE: u32 = 150;
/// Maximum GPU texture uploads per frame to keep UI scrolling smooth.
const THUMB_UPLOADS_PER_FRAME: usize = 6;
/// Debounce delay before fetching a live thumbnail for a typed Paste/ID value.
const PASTE_DEBOUNCE: Duration = Duration::from_millis(320);
/// Safety cap on local file reads for preview decoding (64 MiB).
const MAX_LOCAL_PREVIEW_BYTES: u64 = 64 * 1024 * 1024;

/// Target property metadata captured when opening the Asset Picker.
#[derive(Clone, Debug)]
pub struct PickerTarget {
    /// Selected instances to apply the chosen asset URI to.
    pub targets: Vec<Ref>,
    /// Primary class name of the inspected instance (e.g. "Decal", "Sound", "MeshPart").
    pub class_name: String,
    /// Property name (e.g. "Image", "Texture", "SoundId", "AudioContent", "MeshId").
    pub prop_name: String,
    /// `true` for `Variant::Content`, `false` for `Variant::ContentId`.
    pub is_content_variant: bool,
}

/// Completed selection returned by [`AssetPicker::show`] to be committed to the document.
#[derive(Clone, Debug)]
pub struct PickerApply {
    pub target: PickerTarget,
    pub uri: String,
}

/// Context-aware asset category inferred from the target property / class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PickerCategory {
    #[default]
    Image,
    Audio,
    Mesh,
    Animation,
}

impl PickerCategory {
    /// Infers the appropriate asset category from the property name and instance class.
    pub fn infer(prop_name: &str, class_name: &str) -> Self {
        let p = prop_name.to_ascii_lowercase();
        let c = class_name.to_ascii_lowercase();

        // Audio properties
        if p == "soundid"
            || p == "audiocontent"
            || (p == "asset" && c == "audioplayer")
            || c == "sound"
            || c == "audioplayer"
        {
            return Self::Audio;
        }

        // Animation properties
        if p == "animationid" || c == "animation" {
            return Self::Animation;
        }

        // Mesh properties
        if p == "meshid"
            || p == "meshcontent"
            || p == "cagemeshid"
            || p == "referencemeshid"
            || p == "cagemeshcontent"
            || p == "referencemeshcontent"
            || ((c == "meshpart" || c == "specialmesh" || c == "filemesh")
                && (p.contains("mesh") && !p.contains("texture")))
        {
            return Self::Mesh;
        }

        // Image / Decal / Texture / Skybox / Clothing properties
        if p == "image"
            || p == "imagecontent"
            || p == "texture"
            || p == "textureid"
            || p == "texturecontent"
            || p == "hoverimage"
            || p == "pressedimage"
            || p.starts_with("skybox")
            || p == "shirttemplate"
            || p == "pantstemplate"
            || p == "colormap"
            || p == "normalmap"
            || p == "metalnessmap"
            || p == "roughnessmap"
            || p == "colormapcontent"
            || p == "normalmapcontent"
            || p == "metalnessmapcontent"
            || p == "roughnessmapcontent"
            || p == "icon"
            || p == "face"
            || p == "graphic"
            || p == "cursoricon"
            || p.contains("image")
            || p.contains("texture")
            || matches!(
                c.as_str(),
                "decal"
                    | "texture"
                    | "imagelabel"
                    | "imagebutton"
                    | "sky"
                    | "shirt"
                    | "pants"
                    | "surfaceappearance"
                    | "particleemitter"
                    | "beam"
                    | "trail"
            )
        {
            return Self::Image;
        }

        Self::Image
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Image => "Image / Decal",
            Self::Audio => "Audio",
            Self::Mesh => "Mesh / 3D Model",
            Self::Animation => "Animation",
        }
    }

    pub fn filter_label(self) -> &'static str {
        match self {
            Self::Image => "Images (png, jpg, webp, bmp, tga)",
            Self::Audio => "Audio (mp3, ogg, wav, flac)",
            Self::Mesh => "3D Meshes (obj, fbx, glb, gltf, mesh)",
            Self::Animation => "Animations (rbxm, rbxmx, fbx)",
        }
    }

    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Image => &["png", "jpg", "jpeg", "webp", "bmp", "tga"],
            Self::Audio => &["mp3", "ogg", "wav", "flac"],
            Self::Mesh => &["obj", "fbx", "glb", "gltf", "mesh"],
            Self::Animation => &["rbxm", "rbxmx", "fbx"],
        }
    }

    /// Roblox `CloudApi::upload_asset` `asset_type` parameter.
    pub fn upload_asset_type(self) -> &'static str {
        match self {
            Self::Image => "Decal",
            Self::Audio => "Audio",
            Self::Mesh => "Model",
            Self::Animation => "Animation",
        }
    }

    /// Default Toolbox search kind for the Browse Roblox tab.
    pub fn toolbox_kind(self) -> ToolboxKind {
        match self {
            Self::Image => ToolboxKind::Decal,
            Self::Audio => ToolboxKind::Audio,
            Self::Mesh => ToolboxKind::MeshPart,
            Self::Animation => ToolboxKind::Animation,
        }
    }
}

/// Active tab inside the Asset Picker modal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PickerTab {
    #[default]
    PasteId,
    Upload,
    Browse,
}

/// Source toggle in the Browse Roblox tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BrowseSource {
    #[default]
    CreatorStore,
    MyCreations,
}

/// Layout toggle in the Browse Roblox tab (Grid default for visuals, List default for Audio).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum BrowseViewMode {
    #[default]
    Grid,
    List,
}

fn default_view_for_kind(kind: ToolboxKind) -> BrowseViewMode {
    match kind {
        ToolboxKind::Audio => BrowseViewMode::List,
        _ => BrowseViewMode::Grid,
    }
}

/// Compact item displayed in the Browse Roblox tab.
#[derive(Clone, Debug)]
struct BrowseItem {
    id: u64,
    name: String,
    creator: String,
}

/// Normalizes user-entered asset input into a Roblox URI.
///
/// Pure numeric input (e.g. `123456789`) becomes `rbxassetid://123456789`.
/// Empty input stays empty (clears the property). Existing URI schemes are
/// preserved verbatim.
pub fn normalize_asset_uri(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return format!("rbxassetid://{trimmed}");
    }
    trimmed.to_string()
}

/// Extracts a numeric Roblox asset ID from raw user input (`12345`,
/// `rbxassetid://12345`, or `...id=12345`).
fn extract_asset_id(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<u64>().ok().filter(|&id| id > 0);
    }
    let lower = s.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("rbxassetid://") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse::<u64>().ok().filter(|&id| id > 0);
        }
    }
    if let Some(pos) = lower.find("id=") {
        let rest = &s[pos + 3..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse::<u64>().ok().filter(|&id| id > 0);
        }
    }
    None
}

fn mime_for_path(path: &Path, cat: PickerCategory) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tga" => "image/tga",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "fbx" => "model/fbx",
        "obj" => "model/obj",
        "gltf" => "model/gltf+json",
        "glb" => "model/gltf-binary",
        "rbxm" => "model/x-rbxm",
        "rbxmx" => "model/x-rbxmx",
        _ => match cat {
            PickerCategory::Image => "image/png",
            PickerCategory::Audio => "audio/mpeg",
            PickerCategory::Mesh => "model/obj",
            PickerCategory::Animation => "model/x-rbxm",
        },
    }
}

fn is_image_ext(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tga")
}

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", (bytes + 512) / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Decodes image bytes on a worker thread, returning a thumbnail-resized RGBA
/// `ColorImage` (capped at `max_dim × max_dim`) plus the original `(W, H)`.
fn decode_resized_rgba(bytes: &[u8], max_dim: u32) -> Option<(egui::ColorImage, (u32, u32))> {
    let img = image::load_from_memory(bytes).ok()?;
    let orig_w = img.width();
    let orig_h = img.height();
    if orig_w == 0 || orig_h == 0 || orig_w > 8192 || orig_h > 8192 {
        return None;
    }
    let resized = if orig_w > max_dim || orig_h > max_dim {
        img.thumbnail(max_dim, max_dim)
    } else {
        img
    };
    let rgba = resized.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some((
        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw()),
        (orig_w, orig_h),
    ))
}

#[cfg(target_os = "android")]
fn read_system_clipboard() -> Option<String> {
    crate::android::clipboard::text_now().or_else(crate::android::clipboard::text)
}

#[cfg(not(target_os = "android"))]
fn read_system_clipboard() -> Option<String> {
    #[cfg(target_os = "macos")]
    let cmds: &[(&str, &[&str])] = &[("pbpaste", &[])];
    #[cfg(target_os = "linux")]
    let cmds: &[(&str, &[&str])] = &[
        ("wl-paste", &["-n"]),
        ("xclip", &["-o", "-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--output"]),
    ];
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let cmds: &[(&str, &[&str])] = &[];

    for (bin, args) in cmds {
        if let Ok(out) = std::process::Command::new(bin).args(*args).output() {
            if out.status.success() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Bounded RAM-safe thumbnail cache (max 48 TextureHandles, cleared on close/search)
// ---------------------------------------------------------------------------

enum ThumbEntry {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

struct ThumbDecoded {
    gen: u64,
    id: u64,
    image: Option<egui::ColorImage>,
}

struct ThumbCache {
    entries: HashMap<u64, (ThumbEntry, u64)>,
    tick: u64,
    pending_uploads: VecDeque<ThumbDecoded>,
    tx: Sender<ThumbDecoded>,
    rx: Receiver<ThumbDecoded>,
    cancels: Vec<Arc<AtomicBool>>,
    gen: u64,
    inflight: usize,
}

impl Default for ThumbCache {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            entries: HashMap::new(),
            tick: 0,
            pending_uploads: VecDeque::new(),
            tx,
            rx,
            cancels: Vec::new(),
            gen: 0,
            inflight: 0,
        }
    }
}

impl Drop for ThumbCache {
    fn drop(&mut self) {
        self.clear();
    }
}

impl ThumbCache {
    /// Immediately drops 100% of cached `TextureHandle`s, pending decoded images,
    /// and signals any in-flight thumbnail workers to stop.
    fn clear(&mut self) {
        for flag in self.cancels.drain(..) {
            flag.store(true, Ordering::Relaxed);
        }
        self.gen = self.gen.wrapping_add(1);
        self.entries.clear();
        self.entries.shrink_to_fit();
        self.pending_uploads.clear();
        self.pending_uploads.shrink_to_fit();
        while self.rx.try_recv().is_ok() {}
        self.inflight = 0;
    }

    fn busy(&self) -> bool {
        self.inflight > 0 || !self.pending_uploads.is_empty()
    }

    fn peek(&self, id: u64) -> Option<&ThumbEntry> {
        self.entries.get(&id).map(|(e, _)| e)
    }

    fn enforce_cap(&mut self) {
        while self.entries.len() > THUMB_CACHE_CAP {
            // Prefer evicting the oldest non-Loading entry; fall back to oldest overall.
            let victim = self
                .entries
                .iter()
                .filter(|(_, (e, _))| !matches!(e, ThumbEntry::Loading))
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| *k)
                .or_else(|| {
                    self.entries
                        .iter()
                        .min_by_key(|(_, (_, used))| *used)
                        .map(|(k, _)| *k)
                });
            if let Some(k) = victim {
                self.entries.remove(&k);
            } else {
                break;
            }
        }
    }

    /// Spawns a background worker to batch-fetch thumbnails from Roblox's
    /// `thumbnails.roblox.com/v1/assets` API, decode & resize each to <= 128×128
    /// RGBA on the background thread, and stream `(id, ColorImage)` to the UI.
    fn fetch_ids(&mut self, ids: &[u64], ctx: &egui::Context) {
        let mut needed: Vec<u64> = Vec::new();
        for &id in ids {
            if id == 0 || self.entries.contains_key(&id) || needed.contains(&id) {
                continue;
            }
            needed.push(id);
            if needed.len() >= THUMB_CACHE_CAP {
                break;
            }
        }
        if needed.is_empty() {
            return;
        }

        for &id in &needed {
            self.tick = self.tick.wrapping_add(1);
            self.entries.insert(id, (ThumbEntry::Loading, self.tick));
        }
        self.enforce_cap();
        self.inflight += needed.len();

        let gen = self.gen;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.push(cancel.clone());

        let needed_worker = needed.clone();
        let spawned = std::thread::Builder::new()
            .name("asset-picker-thumbs".into())
            .spawn(move || {
                let http: Box<dyn Http> = Box::new(AnonHttp);
                let is_cancelled = || cancel.load(Ordering::Relaxed);

                for chunk in needed_worker.chunks(tb_api::DETAILS_CHUNK) {
                    if is_cancelled() {
                        return;
                    }
                    let url = tb_api::thumbnails_url(chunk, THUMB_FETCH_SIZE);
                    let listed = tb_api::get_bytes(&*http, &url, false, &is_cancelled)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                        .map(|v| tb_model::parse_thumbnails(&v))
                        .unwrap_or_default();

                    for &id in chunk {
                        if is_cancelled() {
                            return;
                        }
                        let image = match listed.iter().find(|r| r.0 == id) {
                            Some((_, state, Some(img_url))) if state == "Completed" => {
                                tb_api::get_bytes(&*http, img_url, false, &is_cancelled)
                                    .ok()
                                    .and_then(|bytes| decode_resized_rgba(&bytes, THUMB_MAX_DIM))
                                    .map(|(img, _)| img)
                            }
                            _ => None,
                        };
                        let _ = tx.send(ThumbDecoded { gen, id, image });
                        ctx.request_repaint();
                    }
                }
            });

        if spawned.is_err() {
            for id in needed {
                self.inflight = self.inflight.saturating_sub(1);
                self.tick = self.tick.wrapping_add(1);
                self.entries.insert(id, (ThumbEntry::Failed, self.tick));
            }
        }
    }

    /// Drains decoded thumbnails from the worker channel and uploads up to
    /// `THUMB_UPLOADS_PER_FRAME` textures per frame.
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            if msg.gen == self.gen {
                self.pending_uploads.push_back(msg);
            }
        }

        let mut uploaded = 0;
        while uploaded < THUMB_UPLOADS_PER_FRAME {
            let Some(msg) = self.pending_uploads.pop_front() else {
                break;
            };
            self.inflight = self.inflight.saturating_sub(1);
            let entry = match msg.image {
                Some(color_img) => {
                    uploaded += 1;
                    ThumbEntry::Ready(ctx.load_texture(
                        format!("asset-picker-thumb-{}-{}", self.gen, msg.id),
                        color_img,
                        egui::TextureOptions::LINEAR,
                    ))
                }
                None => ThumbEntry::Failed,
            };
            self.tick = self.tick.wrapping_add(1);
            self.entries.insert(msg.id, (entry, self.tick));
            self.enforce_cap();
        }

        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

// ---------------------------------------------------------------------------
// Local upload preview state
// ---------------------------------------------------------------------------

struct LocalPreviewDecoded {
    path: PathBuf,
    file_size: u64,
    orig_dims: Option<(u32, u32)>,
    image: Option<egui::ColorImage>,
    note: Option<String>,
}

struct LocalPreviewState {
    file_size: u64,
    orig_dims: Option<(u32, u32)>,
    texture: Option<egui::TextureHandle>,
    loading: bool,
    note: Option<String>,
}

// ---------------------------------------------------------------------------
// Visual painting helpers (Godot-style preview boxes, checkerboard, audio bars)
// ---------------------------------------------------------------------------

fn paint_checkerboard(painter: &egui::Painter, rect: Rect, cell: f32) {
    let clipped = painter.with_clip_rect(painter.clip_rect().intersect(rect));
    let c0 = Color32::from_rgb(28, 30, 36);
    let c1 = Color32::from_rgb(38, 41, 49);
    clipped.rect_filled(rect, 4.0, c0);
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
    for r in 0..rows {
        for c in 0..cols {
            if (r + c) & 1 == 1 {
                let x = rect.min.x + c as f32 * cell;
                let y = rect.min.y + r as f32 * cell;
                let sq = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(cell)).intersect(rect);
                clipped.rect_filled(sq, 0.0, c1);
            }
        }
    }
}

fn paint_audio_waveform(painter: &egui::Painter, rect: Rect, accent: Color32) {
    painter.rect_filled(rect, 4.0, Color32::from_rgb(27, 32, 42));
    let heights: [f32; 9] = [0.22, 0.45, 0.72, 0.42, 0.88, 0.56, 0.76, 0.38, 0.24];
    let n = heights.len() as f32;
    let inner = rect.shrink2(Vec2::new(rect.width() * 0.16, rect.height() * 0.20));
    let step = inner.width() / n;
    let bar_w = (step * 0.56).max(2.0);
    let cy = inner.center().y;
    for (i, &h_frac) in heights.iter().enumerate() {
        let cx = inner.min.x + step * (i as f32 + 0.5);
        let half_h = (inner.height() * h_frac * 0.5).max(2.0);
        let bar = Rect::from_center_size(Pos2::new(cx, cy), Vec2::new(bar_w, half_h * 2.0));
        painter.rect_filled(bar, bar_w * 0.5, accent);
    }
}

fn paint_fitted_texture(painter: &egui::Painter, rect: Rect, tex: &egui::TextureHandle) {
    let s = tex.size_vec2();
    if s.x <= 0.0 || s.y <= 0.0 {
        return;
    }
    let inset = rect.shrink(3.0);
    let k = (inset.width() / s.x).min(inset.height() / s.y);
    let ir = Rect::from_center_size(inset.center(), s * k);
    painter.image(
        tex.id(),
        ir,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

fn paint_thumb_box(
    ui: &egui::Ui,
    rect: Rect,
    entry: Option<&ThumbEntry>,
    is_audio: bool,
    empty_label: &str,
) {
    let painter = ui.painter();
    match entry {
        Some(ThumbEntry::Ready(tex)) => {
            paint_checkerboard(painter, rect, 8.0);
            paint_fitted_texture(painter, rect, tex);
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, Color32::from_rgb(56, 61, 74)),
                StrokeKind::Inside,
            );
        }
        Some(ThumbEntry::Loading) => {
            if is_audio {
                paint_audio_waveform(painter, rect, Color32::from_rgb(76, 130, 210));
            } else {
                // Subtle animated shimmer box while thumbnail loads
                let t = ui.input(|i| i.time) as f32;
                let pulse = ((t * 3.0).sin() * 0.5 + 0.5) * 14.0;
                let base = 32 + pulse as u8;
                painter.rect_filled(rect, 4.0, Color32::from_rgb(base, base + 2, base + 8));
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "...",
                    FontId::proportional(13.0),
                    Color32::from_rgb(130, 138, 156),
                );
            }
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, Color32::from_rgb(52, 56, 68)),
                StrokeKind::Inside,
            );
        }
        Some(ThumbEntry::Failed) => {
            if is_audio {
                paint_audio_waveform(painter, rect, Color32::from_rgb(86, 156, 245));
            } else {
                painter.rect_filled(rect, 4.0, Color32::from_rgb(30, 32, 39));
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    empty_label,
                    FontId::proportional(11.0),
                    Color32::from_rgb(120, 126, 142),
                );
            }
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, Color32::from_rgb(52, 56, 68)),
                StrokeKind::Inside,
            );
        }
        None => {
            if is_audio {
                paint_audio_waveform(painter, rect, Color32::from_rgb(86, 156, 245));
            } else {
                painter.rect_filled(rect, 4.0, Color32::from_rgb(28, 30, 36));
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    empty_label,
                    FontId::proportional(11.0),
                    Color32::from_rgb(112, 118, 134),
                );
            }
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, Color32::from_rgb(50, 54, 66)),
                StrokeKind::Inside,
            );
        }
    }
}

/// Modal state for the Godot/Roblox Studio-style Asset Picker.
#[derive(Default)]
pub struct AssetPicker {
    pub open: bool,
    target: Option<PickerTarget>,
    category: PickerCategory,
    tab: PickerTab,

    // Tab 1: Paste / ID
    current_uri: String,
    input_uri: String,
    paste_notice: Option<String>,
    paste_preview_id: Option<u64>,
    paste_debounce_until: Option<Instant>,

    // Tab 2: Upload to Roblox
    selected_file: Option<PathBuf>,
    asset_name: String,
    uploading: bool,
    upload_rx: Option<Receiver<Result<u64, String>>>,
    upload_error: Option<String>,
    local_preview: Option<LocalPreviewState>,
    local_preview_rx: Option<Receiver<LocalPreviewDecoded>>,

    // Native / Android file picker owned by the Asset Picker
    #[cfg(target_os = "android")]
    file_picker: Option<crate::android::storage::Picker>,
    #[cfg(not(target_os = "android"))]
    file_picker_rx: Option<Receiver<Option<PathBuf>>>,

    // Tab 3: Browse Roblox (Visual Card Grid / List)
    browse_source: BrowseSource,
    browse_kind: ToolboxKind,
    browse_view: BrowseViewMode,
    search_query: String,
    browse_loading: bool,
    browse_fetched_once: bool,
    browse_rx: Option<Receiver<Result<Vec<BrowseItem>, String>>>,
    browse_items: Vec<BrowseItem>,
    browse_error: Option<String>,
    selected_browse_id: Option<u64>,

    // Shared bounded RAM-safe thumbnail cache (max 48 TextureHandles)
    thumbs: ThumbCache,
}

impl AssetPicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when background upload, search, thumbnail, or preview work is in flight.
    pub fn busy(&self) -> bool {
        self.uploading
            || self.browse_loading
            || self.thumbs.busy()
            || self.local_preview.as_ref().is_some_and(|p| p.loading)
            || self.paste_debounce_until.is_some()
    }

    /// Opens the Asset Picker modal for the given target instances and property.
    pub fn open_for(
        &mut self,
        targets: Vec<Ref>,
        class_name: String,
        prop_name: String,
        is_content_variant: bool,
        current_uri: String,
    ) {
        // Release any leftover GPU/CPU textures before starting fresh.
        self.thumbs.clear();
        self.local_preview = None;
        self.local_preview_rx = None;

        let category = PickerCategory::infer(&prop_name, &class_name);
        self.open = true;
        self.category = category;
        self.tab = PickerTab::PasteId;
        self.current_uri = current_uri.clone();
        self.input_uri = current_uri;
        self.paste_notice = None;
        self.paste_preview_id = extract_asset_id(&self.input_uri);
        self.paste_debounce_until = self.paste_preview_id.map(|_| Instant::now());

        self.selected_file = None;
        self.asset_name.clear();
        self.uploading = false;
        self.upload_rx = None;
        self.upload_error = None;
        #[cfg(target_os = "android")]
        {
            self.file_picker = None;
            crate::android::clipboard::prime();
        }
        #[cfg(not(target_os = "android"))]
        {
            self.file_picker_rx = None;
        }

        self.browse_source = BrowseSource::CreatorStore;
        self.browse_kind = category.toolbox_kind();
        self.browse_view = default_view_for_kind(self.browse_kind);
        self.search_query.clear();
        self.browse_loading = false;
        self.browse_fetched_once = false;
        self.browse_rx = None;
        self.browse_items.clear();
        self.browse_error = None;
        self.selected_browse_id = None;

        self.target = Some(PickerTarget {
            targets,
            class_name,
            prop_name,
            is_content_variant,
        });
    }

    /// Closes the modal and immediately drops 100% of cached GPU/CPU textures
    /// and transient worker receivers.
    pub fn close(&mut self) {
        self.open = false;
        self.uploading = false;
        self.upload_rx = None;
        self.browse_loading = false;
        self.browse_rx = None;
        self.selected_browse_id = None;
        self.paste_preview_id = None;
        self.paste_debounce_until = None;
        self.local_preview = None;
        self.local_preview_rx = None;
        self.thumbs.clear();
        #[cfg(target_os = "android")]
        {
            self.file_picker = None;
        }
        #[cfg(not(target_os = "android"))]
        {
            self.file_picker_rx = None;
        }
    }

    /// Starts the platform-appropriate non-blocking file picker for the Upload tab.
    fn start_file_picker(&mut self, ctx: &egui::Context) {
        let req = FileDialogRequest::new()
            .set_title(&format!("Choose {} File", self.category.label()))
            .add_filter(self.category.filter_label(), self.category.extensions());

        #[cfg(target_os = "android")]
        {
            self.file_picker = Some(crate::android::storage::Picker::new(req, FileDialogKind::PickFile));
            ctx.request_repaint();
        }

        #[cfg(not(target_os = "android"))]
        {
            if self.file_picker_rx.is_some() {
                return;
            }
            let dialog = req.into_rfd();
            let (tx, rx) = mpsc::channel();
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new()
                .name("asset-picker-file".into())
                .spawn(move || {
                    let path = pollster::block_on(dialog.pick_file()).map(|h| h.path().to_path_buf());
                    let _ = tx.send(path);
                    ctx.request_repaint();
                });
            self.file_picker_rx = Some(rx);
        }
    }

    /// Polls the file picker and updates `selected_file` / `asset_name` / local preview when chosen.
    /// Returns `true` if the Android in-app file picker is currently displayed on screen.
    fn poll_file_picker(&mut self, ctx: &egui::Context) -> bool {
        #[cfg(target_os = "android")]
        {
            if let Some(picker) = self.file_picker.as_mut() {
                if let Some(chosen) = picker.show(ctx) {
                    self.file_picker = None;
                    if let Some(path) = chosen {
                        self.set_selected_file(path, ctx);
                    }
                    ctx.request_repaint();
                    return false;
                }
                return true;
            }
            false
        }

        #[cfg(not(target_os = "android"))]
        {
            let done = match self.file_picker_rx.as_ref() {
                Some(rx) => match rx.try_recv() {
                    Ok(opt) => Some(opt),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(None),
                },
                None => None,
            };
            if let Some(chosen) = done {
                self.file_picker_rx = None;
                if let Some(path) = chosen {
                    self.set_selected_file(path, ctx);
                }
            }
            false
        }
    }

    fn set_selected_file(&mut self, path: PathBuf, ctx: &egui::Context) {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Asset")
            .to_string();
        self.asset_name = stem;
        self.selected_file = Some(path.clone());
        self.upload_error = None;

        // Drop previous local preview texture immediately before decoding the new file.
        self.local_preview = Some(LocalPreviewState {
            file_size: 0,
            orig_dims: None,
            texture: None,
            loading: true,
            note: None,
        });
        self.local_preview_rx = None;

        let (tx, rx) = mpsc::channel();
        let ctx_clone = ctx.clone();
        self.local_preview_rx = Some(rx);

        let spawned = std::thread::Builder::new()
            .name("asset-picker-local-preview".into())
            .spawn(move || {
                let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .unwrap_or_default();

                let (orig_dims, image, note) = if is_image_ext(&ext) {
                    if file_size > MAX_LOCAL_PREVIEW_BYTES {
                        (None, None, Some("File too large for live preview".into()))
                    } else {
                        match std::fs::read(&path) {
                            Ok(bytes) => match decode_resized_rgba(&bytes, LOCAL_PREVIEW_MAX_DIM) {
                                Some((color_img, dims)) => (Some(dims), Some(color_img), None),
                                None => (
                                    None,
                                    None,
                                    Some(format!("Preview unavailable for .{ext} (ready to upload)")),
                                ),
                            },
                            Err(e) => (None, None, Some(format!("Could not read preview: {e}"))),
                        }
                    }
                } else {
                    (None, None, None)
                };

                let _ = tx.send(LocalPreviewDecoded {
                    path,
                    file_size,
                    orig_dims,
                    image,
                    note,
                });
                ctx_clone.request_repaint();
            });

        if spawned.is_err() {
            if let Some(lp) = self.local_preview.as_mut() {
                lp.loading = false;
            }
            self.local_preview_rx = None;
        }
    }

    fn poll_local_preview(&mut self, ctx: &egui::Context) {
        let done = match self.local_preview_rx.as_ref() {
            Some(rx) => match rx.try_recv() {
                Ok(decoded) => Some(Some(decoded)),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                    None
                }
                Err(TryRecvError::Disconnected) => Some(None),
            },
            None => None,
        };

        if let Some(opt_decoded) = done {
            self.local_preview_rx = None;
            match opt_decoded {
                Some(decoded) if self.selected_file.as_ref() == Some(&decoded.path) => {
                    let texture = decoded.image.map(|color_img| {
                        ctx.load_texture(
                            "asset-picker-local-preview",
                            color_img,
                            egui::TextureOptions::LINEAR,
                        )
                    });
                    self.local_preview = Some(LocalPreviewState {
                        file_size: decoded.file_size,
                        orig_dims: decoded.orig_dims,
                        texture,
                        loading: false,
                        note: decoded.note,
                    });
                }
                _ => {
                    if let Some(lp) = self.local_preview.as_mut() {
                        lp.loading = false;
                    }
                }
            }
        }
    }

    /// Spawns a background thread to upload the selected file to Roblox.
    fn start_upload(&mut self, session: Session, ctx: &egui::Context) {
        let Some(path) = self.selected_file.clone() else {
            self.upload_error = Some("Choose a file first.".into());
            return;
        };
        let display_name = {
            let trimmed = self.asset_name.trim();
            if trimmed.is_empty() {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Asset")
                    .to_string()
            } else {
                trimmed.to_string()
            }
        };
        let asset_type = self.category.upload_asset_type();
        let mime = mime_for_path(&path, self.category);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("asset")
            .to_string();

        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        self.uploading = true;
        self.upload_error = None;
        self.upload_rx = Some(rx);

        let spawned = std::thread::Builder::new()
            .name("asset-picker-upload".into())
            .spawn(move || {
                let res = std::fs::read(&path)
                    .map_err(|e| format!("Could not read file: {e}"))
                    .and_then(|bytes| {
                        let api = CloudApi {
                            session: session.downloader(),
                            ep: Endpoints::default(),
                        };
                        api.upload_asset(asset_type, &display_name, "", None, &filename, mime, &bytes)
                            .map_err(|e| e.0)
                    });
                let _ = tx.send(res);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.uploading = false;
            self.upload_rx = None;
            self.upload_error = Some(format!("Could not start upload thread: {e}"));
        }
    }

    /// Spawns a background thread to fetch Creator Store / My Creations results.
    /// Immediately drops all prior thumbnail textures to keep RAM strictly bounded.
    fn start_browse(&mut self, session: Option<Session>, ctx: &egui::Context) {
        // Strict RAM safety: drop all prior browse textures whenever a new search starts.
        self.thumbs.clear();
        self.selected_browse_id = None;

        let source = match self.browse_source {
            BrowseSource::CreatorStore => Source::Store,
            BrowseSource::MyCreations => {
                if session.is_none() {
                    self.browse_loading = false;
                    self.browse_fetched_once = true;
                    self.browse_items.clear();
                    self.browse_error = Some("Sign in to Roblox to browse My Creations.".into());
                    return;
                }
                Source::Creations { group: None }
            }
        };

        let req = ListReq {
            source,
            kind: self.browse_kind,
            query: self.search_query.trim().to_string(),
            category_path: None,
            sub_type: None,
            filters: Filters::default(),
        };

        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        self.browse_loading = true;
        self.browse_fetched_once = true;
        self.browse_error = None;
        self.browse_rx = Some(rx);

        let spawned = std::thread::Builder::new()
            .name("asset-picker-browse".into())
            .spawn(move || {
                let res: Result<Vec<BrowseItem>, String> = match session {
                    Some(sess) => {
                        let http = SessionHttp(sess);
                        tb_api::fetch_page(&http, &req, None, &|| false)
                            .map(|page| {
                                page.assets
                                    .into_iter()
                                    .map(|a| BrowseItem {
                                        id: a.id,
                                        name: if a.name.is_empty() {
                                            format!("Asset {}", a.id)
                                        } else {
                                            a.name
                                        },
                                        creator: a.creator.name,
                                    })
                                    .collect()
                            })
                            .map_err(|e| e.describe())
                    }
                    None => {
                        // Signed out: attempt anonymous public Creator Store search via AnonHttp.
                        let http = AnonHttp;
                        let url = tb_api::list_url(&req, 0, None);
                        match tb_api::get_json(&http, &url, &|| false) {
                            Ok(v) => {
                                let v2 = url.contains("/v2/assets:search");
                                let page = if v2 {
                                    tb_model::parse_v2_search(&v)
                                } else {
                                    tb_model::parse_id_list(&v)
                                };
                                let items: Vec<BrowseItem> = if !page.assets.is_empty() {
                                    page.assets
                                        .into_iter()
                                        .map(|a| BrowseItem {
                                            id: a.id,
                                            name: if a.name.is_empty() {
                                                format!("Asset {}", a.id)
                                            } else {
                                                a.name
                                            },
                                            creator: a.creator.name,
                                        })
                                        .collect()
                                } else {
                                    page.ids
                                        .into_iter()
                                        .map(|id| BrowseItem {
                                            id,
                                            name: format!("Asset {id}"),
                                            creator: String::new(),
                                        })
                                        .collect()
                                };
                                Ok(items)
                            }
                            Err(Failure::SignedOut | Failure::Forbidden(_)) => Err(
                                "Sign in to Roblox to browse the Creator Store, or paste an asset ID in the Paste / ID tab.".into(),
                            ),
                            Err(e) => Err(e.describe()),
                        }
                    }
                };
                let _ = tx.send(res);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            self.browse_loading = false;
            self.browse_rx = None;
            self.browse_error = Some(format!("Could not start search thread: {e}"));
        }
    }

    /// Renders and polls the Asset Picker modal each frame.
    ///
    /// Returns `Some(PickerApply)` when the user applies a pasted ID, completes
    /// a Roblox upload, or selects an asset from the ID Browser.
    pub fn show(&mut self, ctx: &egui::Context, session: Option<Session>) -> Option<PickerApply> {
        if !self.open {
            return None;
        }

        // Poll background upload first so completion applies even while switching tabs.
        let mut applied_uri: Option<String> = None;
        if let Some(rx) = self.upload_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(asset_id)) => {
                    self.uploading = false;
                    self.upload_rx = None;
                    self.upload_error = None;
                    applied_uri = Some(format!("rbxassetid://{asset_id}"));
                }
                Ok(Err(err)) => {
                    self.uploading = false;
                    self.upload_rx = None;
                    self.upload_error = Some(err);
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                Err(TryRecvError::Disconnected) => {
                    self.uploading = false;
                    self.upload_rx = None;
                    self.upload_error = Some("Upload worker stopped unexpectedly.".into());
                }
            }
        }

        // Poll background Creator Store / My Creations search.
        let mut newly_arrived_ids: Option<Vec<u64>> = None;
        if let Some(rx) = self.browse_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(items)) => {
                    self.browse_loading = false;
                    self.browse_rx = None;
                    self.browse_error = None;
                    let ids: Vec<u64> = items.iter().take(THUMB_CACHE_CAP).map(|i| i.id).collect();
                    self.browse_items = items;
                    newly_arrived_ids = Some(ids);
                }
                Ok(Err(err)) => {
                    self.browse_loading = false;
                    self.browse_rx = None;
                    self.browse_error = Some(err);
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                Err(TryRecvError::Disconnected) => {
                    self.browse_loading = false;
                    self.browse_rx = None;
                    self.browse_error = Some("Search worker stopped unexpectedly.".into());
                }
            }
        }

        if let Some(ids) = newly_arrived_ids {
            if !ids.is_empty() {
                self.thumbs.fetch_ids(&ids, ctx);
            }
        }

        // Poll debounced Paste/ID thumbnail fetch.
        if self.tab == PickerTab::PasteId {
            if let Some(deadline) = self.paste_debounce_until {
                let now = Instant::now();
                if now >= deadline {
                    self.paste_debounce_until = None;
                    if let Some(id) = self.paste_preview_id {
                        self.thumbs.fetch_ids(&[id], ctx);
                    }
                } else {
                    ctx.request_repaint_after(deadline.saturating_duration_since(now));
                }
            }
        }

        // Poll local image preview decoder and remote thumbnail uploads.
        self.poll_local_preview(ctx);
        self.thumbs.poll(ctx);

        if let Some(uri) = applied_uri {
            let target = self.target.clone()?;
            self.close();
            return Some(PickerApply { target, uri });
        }

        // On Android, while the in-app storage Picker is open, draw it cleanly
        // without the Asset Picker modal obscuring it on small phone screens.
        if self.poll_file_picker(ctx) {
            return None;
        }

        let target = self.target.clone()?;
        let screen = ctx.content_rect();
        let win_w = (screen.width() - 24.0).clamp(320.0, 620.0);
        let win_h = (screen.height() - 24.0).clamp(260.0, 440.0);
        let win_size = egui::vec2(win_w, win_h);

        let mut open = self.open;
        let mut commit_uri: Option<String> = None;
        let mut want_file_picker = false;
        let mut want_upload = false;
        let mut want_browse = false;

        egui::Window::new(format!("Asset Picker - {}", target.prop_name))
            .id(egui::Id::new("rustudio_asset_picker_window"))
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .fixed_size(win_size)
            .min_size(win_size)
            .max_size(win_size)
            .constrain_to(screen.shrink(8.0))
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.spacing_mut().button_padding = egui::vec2(10.0, 6.0);

                // Tab bar
                ui.horizontal_wrapped(|ui| {
                    let prev_tab = self.tab;
                    ui.selectable_value(&mut self.tab, PickerTab::PasteId, "Paste / ID");
                    ui.selectable_value(&mut self.tab, PickerTab::Upload, "Upload to Roblox");
                    ui.selectable_value(&mut self.tab, PickerTab::Browse, "Browse Roblox");
                    if self.tab == PickerTab::Browse
                        && prev_tab != PickerTab::Browse
                        && !self.browse_fetched_once
                    {
                        want_browse = true;
                    }
                    if self.tab == PickerTab::PasteId && prev_tab != PickerTab::PasteId {
                        // Re-trigger thumbnail preview if cache was cleared by a Browse search.
                        if let Some(id) = self.paste_preview_id {
                            if self.thumbs.peek(id).is_none() {
                                self.paste_debounce_until = Some(Instant::now());
                            }
                        }
                    }
                });
                ui.separator();

                match self.tab {
                    PickerTab::PasteId => {
                        egui::ScrollArea::vertical()
                            .id_salt("asset_picker_paste_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Property: {}.{} ({})",
                                target.class_name,
                                target.prop_name,
                                self.category.label()
                            ))
                            .strong(),
                        );
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Current:").weak());
                            let cur_display = if self.current_uri.is_empty() {
                                "(empty)"
                            } else {
                                self.current_uri.as_str()
                            };
                            ui.label(RichText::new(cur_display).monospace());
                        });
                        ui.add_space(10.0);

                        // Side-by-side input controls + live 96×96 thumbnail preview card
                        let preview_card_w = 114.0_f32;
                        let avail_w = ui.available_width();
                        let side_by_side = avail_w >= 340.0;

                        let draw_paste_controls = |ui: &mut egui::Ui,
                                                   input_uri: &mut String,
                                                   paste_notice: &mut Option<String>,
                                                   paste_preview_id: &mut Option<u64>,
                                                   paste_debounce_until: &mut Option<Instant>,
                                                   commit_uri: &mut Option<String>| {
                            ui.label("Asset ID or URI (numbers auto-format to rbxassetid://...):");
                            ui.add_space(4.0);
                            let edit = ui.add(
                                egui::TextEdit::singleline(input_uri)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("123456789 or rbxassetid://123456789"),
                            );
                            if edit.changed() {
                                let parsed = extract_asset_id(input_uri);
                                if parsed != *paste_preview_id {
                                    *paste_preview_id = parsed;
                                    *paste_debounce_until =
                                        parsed.map(|_| Instant::now() + PASTE_DEBOUNCE);
                                }
                            }
                            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                *commit_uri = Some(normalize_asset_uri(input_uri));
                            }

                            ui.add_space(8.0);
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Paste").clicked() {
                                    if let Some(clip) = read_system_clipboard() {
                                        *input_uri = normalize_asset_uri(&clip);
                                        *paste_notice = None;
                                        *paste_preview_id = extract_asset_id(input_uri);
                                        *paste_debounce_until =
                                            paste_preview_id.map(|_| Instant::now());
                                    } else {
                                        *paste_notice = Some(
                                            "Clipboard is empty (or paste directly into the box)."
                                                .into(),
                                        );
                                    }
                                }
                                if ui.button("Clear").clicked() {
                                    input_uri.clear();
                                    *paste_notice = None;
                                    *paste_preview_id = None;
                                    *paste_debounce_until = None;
                                }
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("Apply").strong().color(Color32::WHITE),
                                        )
                                        .fill(Color32::from_rgb(38, 108, 224)),
                                    )
                                    .clicked()
                                {
                                    *commit_uri = Some(normalize_asset_uri(input_uri));
                                }
                            });

                            let preview = normalize_asset_uri(input_uri);
                            if !preview.is_empty() && preview != input_uri.trim() {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(format!("Will apply: {preview}"))
                                        .small()
                                        .weak(),
                                );
                            }
                            if let Some(msg) = paste_notice.as_ref() {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(msg)
                                        .small()
                                        .color(Color32::from_rgb(230, 185, 80)),
                                );
                            }
                        };

                        let draw_paste_preview_card =
                            |ui: &mut egui::Ui,
                             preview_id: Option<u64>,
                             debouncing: bool,
                             thumbs: &ThumbCache,
                             is_audio: bool| {
                                egui::Frame::new()
                                    .fill(Color32::from_rgb(32, 35, 43))
                                    .stroke(Stroke::new(1.0, Color32::from_rgb(54, 58, 70)))
                                    .corner_radius(6.0)
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.vertical_centered(|ui| {
                                            let (rect, _) = ui.allocate_exact_size(
                                                Vec2::splat(96.0),
                                                Sense::hover(),
                                            );
                                            if let Some(id) = preview_id {
                                                let entry = if debouncing {
                                                    Some(&ThumbEntry::Loading)
                                                } else {
                                                    thumbs.peek(id)
                                                };
                                                paint_thumb_box(
                                                    ui,
                                                    rect,
                                                    entry,
                                                    is_audio,
                                                    "No preview",
                                                );
                                                ui.add_space(4.0);
                                                ui.label(
                                                    RichText::new(format!("#{id}"))
                                                        .small()
                                                        .monospace()
                                                        .weak(),
                                                );
                                            } else {
                                                paint_thumb_box(
                                                    ui,
                                                    rect,
                                                    None,
                                                    false,
                                                    "Preview",
                                                );
                                                ui.add_space(4.0);
                                                ui.label(
                                                    RichText::new("Enter ID").small().weak(),
                                                );
                                            }
                                        });
                                    });
                            };

                        let is_audio_cat = self.category == PickerCategory::Audio;
                        if side_by_side {
                            ui.horizontal_top(|ui| {
                                let left_w = (avail_w - preview_card_w - 14.0).max(160.0);
                                ui.allocate_ui_with_layout(
                                    Vec2::new(left_w, 0.0),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        draw_paste_controls(
                                            ui,
                                            &mut self.input_uri,
                                            &mut self.paste_notice,
                                            &mut self.paste_preview_id,
                                            &mut self.paste_debounce_until,
                                            &mut commit_uri,
                                        );
                                    },
                                );
                                ui.add_space(6.0);
                                ui.allocate_ui_with_layout(
                                    Vec2::new(preview_card_w, 0.0),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        draw_paste_preview_card(
                                            ui,
                                            self.paste_preview_id,
                                            self.paste_debounce_until.is_some(),
                                            &self.thumbs,
                                            is_audio_cat,
                                        );
                                    },
                                );
                            });
                        } else {
                            draw_paste_controls(
                                ui,
                                &mut self.input_uri,
                                &mut self.paste_notice,
                                &mut self.paste_preview_id,
                                &mut self.paste_debounce_until,
                                &mut commit_uri,
                            );
                            ui.add_space(8.0);
                            draw_paste_preview_card(
                                ui,
                                self.paste_preview_id,
                                self.paste_debounce_until.is_some(),
                                &self.thumbs,
                                is_audio_cat,
                            );
                        }
                            });
                    }

                    PickerTab::Upload => {
                        egui::ScrollArea::vertical()
                            .id_salt("asset_picker_upload_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "Upload {} to Roblox ({})",
                                self.category.label(),
                                self.category.upload_asset_type()
                            ))
                            .strong(),
                        );
                        ui.label(
                            RichText::new(format!("Accepted files: {}", self.category.filter_label()))
                                .small()
                                .weak(),
                        );
                        ui.add_space(4.0);

                        if session.is_none() {
                            ui.label(
                                RichText::new("Sign in to Roblox first to upload assets.")
                                    .color(Color32::from_rgb(235, 175, 70)),
                            );
                            ui.add_space(6.0);
                        }

                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(!self.uploading, egui::Button::new("Choose File..."))
                                .clicked()
                            {
                                want_file_picker = true;
                            }
                            if self.selected_file.is_none() {
                                ui.label(RichText::new("No file selected").weak());
                            }
                        });

                        ui.add_space(6.0);

                        // Godot-style local file preview card
                        if let Some(path) = &self.selected_file {
                            let fname = path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("selected file");
                            let ext_upper = path
                                .extension()
                                .and_then(|s| s.to_str())
                                .map(|s| s.to_ascii_uppercase())
                                .unwrap_or_else(|| "FILE".into());

                            egui::Frame::new()
                                .fill(Color32::from_rgb(33, 36, 44))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(56, 61, 74)))
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::same(10))
                                .show(ui, |ui| {
                                    ui.horizontal_top(|ui| {
                                        let card_avail_w = ui.available_width().max(220.0);
                                        let preview_side = if card_avail_w < 340.0 { 80.0 } else { 96.0 };
                                        // Left column: Preview box
                                        ui.allocate_ui_with_layout(
                                            Vec2::splat(preview_side),
                                            egui::Layout::top_down(egui::Align::LEFT),
                                            |ui| {
                                                ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
                                                let (rect, _) = ui.allocate_exact_size(
                                                    Vec2::splat(preview_side),
                                                    Sense::hover(),
                                                );
                                                let painter = ui.painter();
                                                if let Some(lp) = &self.local_preview {
                                                    if let Some(tex) = &lp.texture {
                                                        paint_checkerboard(painter, rect, 8.0);
                                                        paint_fitted_texture(painter, rect, tex);
                                                        painter.rect_stroke(
                                                            rect,
                                                            5.0,
                                                            Stroke::new(
                                                                1.0,
                                                                Color32::from_rgb(64, 70, 84),
                                                            ),
                                                            StrokeKind::Inside,
                                                        );
                                                    } else if lp.loading {
                                                        painter.rect_filled(
                                                            rect,
                                                            5.0,
                                                            Color32::from_rgb(28, 30, 37),
                                                        );
                                                        painter.text(
                                                            rect.center(),
                                                            Align2::CENTER_CENTER,
                                                            "Loading...",
                                                            FontId::proportional(12.0),
                                                            Color32::from_rgb(150, 158, 174),
                                                        );
                                                    } else if self.category == PickerCategory::Audio {
                                                        paint_audio_waveform(
                                                            painter,
                                                            rect,
                                                            Color32::from_rgb(86, 156, 245),
                                                        );
                                                    } else {
                                                        painter.rect_filled(
                                                            rect,
                                                            5.0,
                                                            Color32::from_rgb(28, 30, 37),
                                                        );
                                                        painter.text(
                                                            rect.center(),
                                                            Align2::CENTER_CENTER,
                                                            &ext_upper,
                                                            FontId::proportional(16.0),
                                                            Color32::from_rgb(138, 166, 212),
                                                        );
                                                        painter.rect_stroke(
                                                            rect,
                                                            5.0,
                                                            Stroke::new(
                                                                1.0,
                                                                Color32::from_rgb(56, 61, 74),
                                                            ),
                                                            StrokeKind::Inside,
                                                        );
                                                    }
                                                }
                                            },
                                        );

                                        ui.add_space(10.0);

                                        // Right column: Metadata + editable Asset Name + Upload button
                                        let right_w = (card_avail_w - preview_side - 14.0).max(120.0);
                                        ui.vertical(|ui| {
                                                ui.set_width(right_w);
                                                ui.add(
                                                    egui::Label::new(RichText::new(fname).strong())
                                                        .truncate(),
                                                );
                                                ui.add_space(3.0);

                                                if let Some(lp) = &self.local_preview {
                                                    let mut meta_parts: Vec<String> = Vec::new();
                                                    if let Some((w, h)) = lp.orig_dims {
                                                        meta_parts.push(format!("{w}×{h}"));
                                                    }
                                                    if lp.file_size > 0 {
                                                        meta_parts.push(format_file_size(lp.file_size));
                                                    }
                                                    if !meta_parts.is_empty() {
                                                        ui.label(
                                                            RichText::new(meta_parts.join("  •  "))
                                                                .small()
                                                                .monospace()
                                                                .color(Color32::from_rgb(
                                                                    165, 178, 200,
                                                                )),
                                                        );
                                                    }
                                                    if let Some(note) = &lp.note {
                                                        ui.label(RichText::new(note).small().weak());
                                                    }
                                                }

                                                ui.add_space(6.0);
                                                ui.label(RichText::new("Asset Name:").small().weak());
                                                ui.add_enabled(
                                                    !self.uploading,
                                                    egui::TextEdit::singleline(&mut self.asset_name)
                                                        .desired_width((right_w - 8.0).max(80.0))
                                                        .hint_text("My Asset"),
                                                );

                                                ui.add_space(6.0);
                                                let can_upload = session.is_some()
                                                    && self.selected_file.is_some()
                                                    && !self.uploading;
                                                ui.horizontal(|ui| {
                                                    if ui
                                                        .add_enabled(
                                                            can_upload,
                                                            egui::Button::new(
                                                                RichText::new("Upload & Apply")
                                                                    .strong()
                                                                    .color(Color32::WHITE),
                                                            )
                                                            .fill(Color32::from_rgb(38, 108, 224)),
                                                        )
                                                        .clicked()
                                                    {
                                                        want_upload = true;
                                                    }
                                                    if self.uploading {
                                                        ui.spinner();
                                                        ui.label("Uploading...");
                                                    }
                                                });
                                        });
                                    });
                                });
                        } else {
                            // Empty drop/choose prompt card
                            egui::Frame::new()
                                .fill(Color32::from_rgb(30, 32, 39))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(50, 54, 66)))
                                .corner_radius(8.0)
                                .inner_margin(egui::Margin::same(16))
                                .show(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "Select a local {} file to preview and upload to Roblox.",
                                                self.category.label()
                                            ))
                                            .weak(),
                                        );
                                    });
                                });
                        }

                        if let Some(err) = &self.upload_error {
                            ui.add_space(8.0);
                            ui.label(RichText::new(err).color(Color32::from_rgb(235, 95, 95)));
                        }
                            });
                    }

                    PickerTab::Browse => {
                        // Source toggle (Creator Store vs My Creations) + Asset type filter + Grid/List toggle
                        ui.horizontal_wrapped(|ui| {
                            let prev_src = self.browse_source;
                            ui.selectable_value(
                                &mut self.browse_source,
                                BrowseSource::CreatorStore,
                                "Creator Store",
                            );
                            ui.selectable_value(
                                &mut self.browse_source,
                                BrowseSource::MyCreations,
                                "My Creations",
                            );
                            if self.browse_source != prev_src {
                                want_browse = true;
                            }
                            ui.separator();
                            let prev_kind = self.browse_kind;
                            egui::ComboBox::from_id_salt("asset_picker_browse_kind")
                                .selected_text(match self.browse_kind {
                                    ToolboxKind::Decal => "Images / Decals",
                                    ToolboxKind::Audio => "Audio",
                                    ToolboxKind::MeshPart => "Meshes",
                                    ToolboxKind::Animation => "Animations",
                                    ToolboxKind::Model => "Models",
                                    _ => "Assets",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.browse_kind,
                                        ToolboxKind::Decal,
                                        "Images / Decals",
                                    );
                                    ui.selectable_value(
                                        &mut self.browse_kind,
                                        ToolboxKind::Audio,
                                        "Audio",
                                    );
                                    ui.selectable_value(
                                        &mut self.browse_kind,
                                        ToolboxKind::MeshPart,
                                        "Meshes",
                                    );
                                    ui.selectable_value(
                                        &mut self.browse_kind,
                                        ToolboxKind::Animation,
                                        "Animations",
                                    );
                                    ui.selectable_value(
                                        &mut self.browse_kind,
                                        ToolboxKind::Model,
                                        "Models",
                                    );
                                });
                            if self.browse_kind != prev_kind {
                                self.browse_view = default_view_for_kind(self.browse_kind);
                                want_browse = true;
                            }
                            ui.separator();
                            ui.selectable_value(
                                &mut self.browse_view,
                                BrowseViewMode::Grid,
                                "Grid",
                            );
                            ui.selectable_value(
                                &mut self.browse_view,
                                BrowseViewMode::List,
                                "List",
                            );
                        });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .desired_width((ui.available_width() - 84.0).max(80.0))
                                    .hint_text("Search Roblox assets..."),
                            );
                            let enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui
                                .add_enabled(!self.browse_loading, egui::Button::new("Search"))
                                .clicked()
                                || enter
                            {
                                want_browse = true;
                            }
                        });

                        ui.add_space(6.0);
                        if self.browse_loading {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Searching Roblox...");
                            });
                        }
                        if let Some(err) = &self.browse_error {
                            ui.label(RichText::new(err).color(Color32::from_rgb(235, 110, 110)));
                        }

                        let selected_item = self
                            .selected_browse_id
                            .and_then(|sid| self.browse_items.iter().find(|i| i.id == sid))
                            .cloned();
                        let bottom_bar_reserve = if selected_item.is_some() { 68.0 } else { 8.0 };
                        let list_h = (ui.available_height() - bottom_bar_reserve).max(90.0);
                        let is_audio_kind = self.browse_kind == ToolboxKind::Audio;

                        egui::ScrollArea::vertical()
                            .id_salt("asset_picker_browse_scroll")
                            .max_height(list_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if !self.browse_loading
                                    && self.browse_items.is_empty()
                                    && self.browse_error.is_none()
                                {
                                    ui.label(RichText::new("No matching assets found.").weak());
                                }

                                match self.browse_view {
                                    BrowseViewMode::Grid => {
                                        let avail_w = (ui.available_width() - 6.0).max(140.0);
                                        let gap = 8.0_f32;
                                        let cols = ((avail_w + gap) / 136.0)
                                            .floor()
                                            .clamp(2.0, 5.0)
                                            as usize;
                                        let card_w = ((avail_w - gap * (cols as f32 - 1.0))
                                            / cols as f32)
                                            .floor()
                                            .max(88.0);
                                        let inner_w = (card_w - 12.0).max(76.0);

                                        for row in self.browse_items.chunks(cols) {
                                            ui.horizontal_top(|ui| {
                                                ui.spacing_mut().item_spacing.x = gap;
                                                for item in row {
                                                    let is_selected =
                                                        self.selected_browse_id == Some(item.id);
                                                    let fill = if is_selected {
                                                        Color32::from_rgb(42, 54, 76)
                                                    } else {
                                                        Color32::from_rgb(34, 37, 45)
                                                    };
                                                    let border = if is_selected {
                                                        Stroke::new(
                                                            1.5,
                                                            Color32::from_rgb(76, 148, 255),
                                                        )
                                                    } else {
                                                        Stroke::new(
                                                            1.0,
                                                            Color32::from_rgb(52, 56, 68),
                                                        )
                                                    };

                                                    ui.allocate_ui_with_layout(
                                                        Vec2::new(card_w, 178.0),
                                                        egui::Layout::top_down(egui::Align::LEFT),
                                                        |ui| {
                                                            ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
                                                            egui::Frame::new()
                                                                .fill(fill)
                                                                .stroke(border)
                                                                .corner_radius(6.0)
                                                                .inner_margin(egui::Margin::same(6))
                                                                .show(ui, |ui| {
                                                                    ui.set_width(inner_w);
                                                                    ui.vertical(|ui| {
                                                                // Clickable preview + text region (does not overlap Use button)
                                                                let top_resp = ui
                                                                    .vertical(|ui| {
                                                                        ui.set_width(inner_w);
                                                                        let (thumb_rect, _) = ui
                                                                            .allocate_exact_size(
                                                                                Vec2::new(
                                                                                    inner_w, 82.0,
                                                                                ),
                                                                                Sense::hover(),
                                                                            );
                                                                        paint_thumb_box(
                                                                            ui,
                                                                            thumb_rect,
                                                                            self.thumbs
                                                                                .peek(item.id),
                                                                            is_audio_kind,
                                                                            "No thumb",
                                                                        );

                                                                        ui.add_space(5.0);
                                                                        ui.add(
                                                                            egui::Label::new(
                                                                                RichText::new(
                                                                                    &item.name,
                                                                                )
                                                                                .strong()
                                                                                .size(12.5),
                                                                            )
                                                                            .truncate(),
                                                                        );
                                                                        let creator_str = if item
                                                                            .creator
                                                                            .is_empty()
                                                                        {
                                                                            "Roblox"
                                                                        } else {
                                                                            item.creator.as_str()
                                                                        };
                                                                        ui.add(
                                                                            egui::Label::new(
                                                                                RichText::new(
                                                                                    creator_str,
                                                                                )
                                                                                .small()
                                                                                .weak(),
                                                                            )
                                                                            .truncate(),
                                                                        );
                                                                    })
                                                                    .response;

                                                                let card_click = ui.interact(
                                                                    top_resp.rect,
                                                                    ui.id().with((
                                                                        "asset_picker_card",
                                                                        item.id,
                                                                    )),
                                                                    Sense::click(),
                                                                );
                                                                if card_click.double_clicked() {
                                                                    commit_uri = Some(format!(
                                                                        "rbxassetid://{}",
                                                                        item.id
                                                                    ));
                                                                } else if card_click.clicked() {
                                                                    self.selected_browse_id =
                                                                        Some(item.id);
                                                                }

                                                                ui.add_space(4.0);
                                                                ui.with_layout(
                                                                    egui::Layout::right_to_left(
                                                                        egui::Align::Center,
                                                                    ),
                                                                    |ui| {
                                                                        ui.spacing_mut()
                                                                            .button_padding =
                                                                            Vec2::new(7.0, 2.5);
                                                                        if ui
                                                                            .add(
                                                                                egui::Button::new(
                                                                                    RichText::new("Use")
                                                                                        .size(11.0)
                                                                                        .strong()
                                                                                        .color(Color32::WHITE),
                                                                                )
                                                                                .fill(Color32::from_rgb(
                                                                                    38, 108, 224,
                                                                                )),
                                                                            )
                                                                            .clicked()
                                                                        {
                                                                            commit_uri = Some(
                                                                                format!(
                                                                                    "rbxassetid://{}",
                                                                                    item.id
                                                                                ),
                                                                            );
                                                                        }
                                                                        ui.with_layout(
                                                                            egui::Layout::left_to_right(
                                                                                egui::Align::Center,
                                                                            ),
                                                                            |ui| {
                                                                                ui.add(
                                                                                    egui::Label::new(
                                                                                        RichText::new(format!(
                                                                                            "#{}",
                                                                                            item.id
                                                                                        ))
                                                                                        .size(10.0)
                                                                                        .monospace()
                                                                                        .weak(),
                                                                                    )
                                                                                    .truncate(),
                                                                                );
                                                                            },
                                                                        );
                                                                    },
                                                                );
                                                                    });
                                                                });
                                                        },
                                                    );
                                                }
                                            });
                                            ui.add_space(gap);
                                        }
                                    }

                                    BrowseViewMode::List => {
                                        for item in &self.browse_items {
                                            let is_selected =
                                                self.selected_browse_id == Some(item.id);
                                            let fill = if is_selected {
                                                Color32::from_rgb(42, 54, 76)
                                            } else {
                                                Color32::from_rgb(36, 39, 47)
                                            };
                                            let border = if is_selected {
                                                Stroke::new(1.5, Color32::from_rgb(76, 148, 255))
                                            } else {
                                                Stroke::new(1.0, Color32::from_rgb(50, 54, 66))
                                            };

                                            egui::Frame::new()
                                                .fill(fill)
                                                .stroke(border)
                                                .corner_radius(5.0)
                                                .inner_margin(egui::Margin::symmetric(8, 6))
                                                .show(ui, |ui| {
                                                    ui.horizontal(|ui| {
                                                        // Clickable left info + thumb area
                                                        let left_w =
                                                            (ui.available_width() - 62.0).max(120.0);
                                                        let left_resp = ui
                                                            .allocate_ui_with_layout(
                                                                Vec2::new(left_w, 42.0),
                                                                egui::Layout::left_to_right(
                                                                    egui::Align::Center,
                                                                ),
                                                                |ui| {
                                                                    let (thumb_rect, _) = ui
                                                                        .allocate_exact_size(
                                                                            Vec2::splat(40.0),
                                                                            Sense::hover(),
                                                                        );
                                                                    paint_thumb_box(
                                                                        ui,
                                                                        thumb_rect,
                                                                        self.thumbs.peek(item.id),
                                                                        is_audio_kind,
                                                                        "?",
                                                                    );
                                                                    ui.add_space(6.0);
                                                                    let text_w = (left_w - 50.0).max(60.0);
                                                                    ui.allocate_ui_with_layout(
                                                                        Vec2::new(text_w, 40.0),
                                                                        egui::Layout::top_down(egui::Align::LEFT),
                                                                        |ui| {
                                                                            ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
                                                                            ui.add(
                                                                                egui::Label::new(
                                                                                    RichText::new(
                                                                                        &item.name,
                                                                                    )
                                                                                    .strong(),
                                                                                )
                                                                                .truncate(),
                                                                            );
                                                                            let meta = if item
                                                                                .creator
                                                                                .is_empty()
                                                                            {
                                                                                format!(
                                                                                    "ID: {}",
                                                                                    item.id
                                                                                )
                                                                            } else {
                                                                                format!(
                                                                                    "by {}  •  ID: {}",
                                                                                    item.creator,
                                                                                    item.id
                                                                                )
                                                                            };
                                                                            ui.add(
                                                                                egui::Label::new(
                                                                                    RichText::new(meta)
                                                                                        .small()
                                                                                        .weak(),
                                                                                )
                                                                                .truncate(),
                                                                            );
                                                                        },
                                                                    );
                                                                },
                                                            )
                                                            .response;

                                                        let row_click = ui.interact(
                                                            left_resp.rect,
                                                            ui.id().with((
                                                                "asset_picker_row",
                                                                item.id,
                                                            )),
                                                            Sense::click(),
                                                        );
                                                        if row_click.double_clicked() {
                                                            commit_uri = Some(format!(
                                                                "rbxassetid://{}",
                                                                item.id
                                                            ));
                                                        } else if row_click.clicked() {
                                                            self.selected_browse_id = Some(item.id);
                                                        }

                                                        ui.with_layout(
                                                            egui::Layout::right_to_left(
                                                                egui::Align::Center,
                                                            ),
                                                            |ui| {
                                                                if ui
                                                                    .add(
                                                                        egui::Button::new(
                                                                            RichText::new("Use")
                                                                                .strong()
                                                                                .color(
                                                                                    Color32::WHITE,
                                                                                ),
                                                                        )
                                                                        .fill(Color32::from_rgb(
                                                                            38, 108, 224,
                                                                        )),
                                                                    )
                                                                    .clicked()
                                                                {
                                                                    commit_uri = Some(format!(
                                                                        "rbxassetid://{}",
                                                                        item.id
                                                                    ));
                                                                }
                                                            },
                                                        );
                                                    });
                                                });
                                            ui.add_space(4.0);
                                        }
                                    }
                                }
                            });

                        // Crisp bottom preview / action bar for the currently selected asset
                        if let Some(sel) = selected_item {
                            ui.add_space(6.0);
                            egui::Frame::new()
                                .fill(Color32::from_rgb(30, 34, 44))
                                .stroke(Stroke::new(1.0, Color32::from_rgb(70, 120, 210)))
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let (thumb_rect, _) = ui.allocate_exact_size(
                                            Vec2::splat(40.0),
                                            Sense::hover(),
                                        );
                                        paint_thumb_box(
                                            ui,
                                            thumb_rect,
                                            self.thumbs.peek(sel.id),
                                            is_audio_kind,
                                            "?",
                                        );
                                        ui.add_space(6.0);
                                        let mid_w = (ui.available_width() - 108.0).max(80.0);
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(mid_w, 40.0),
                                            egui::Layout::top_down(egui::Align::LEFT),
                                            |ui| {
                                                ui.set_clip_rect(ui.clip_rect().intersect(ui.max_rect()));
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&sel.name).strong(),
                                                    )
                                                    .truncate(),
                                                );
                                                let subtitle = if sel.creator.is_empty() {
                                                    format!("rbxassetid://{}", sel.id)
                                                } else {
                                                    format!(
                                                        "by {}  •  rbxassetid://{}",
                                                        sel.creator, sel.id
                                                    )
                                                };
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(subtitle)
                                                            .small()
                                                            .monospace()
                                                            .weak(),
                                                    )
                                                    .truncate(),
                                                );
                                            },
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("Apply Asset")
                                                                .strong()
                                                                .color(Color32::WHITE),
                                                        )
                                                        .fill(Color32::from_rgb(38, 108, 224)),
                                                    )
                                                    .clicked()
                                                {
                                                    commit_uri =
                                                        Some(format!("rbxassetid://{}", sel.id));
                                                }
                                            },
                                        );
                                    });
                                });
                        }
                    }
                }
            });

        if want_file_picker {
            self.start_file_picker(ctx);
        }
        if want_upload {
            if let Some(sess) = session {
                self.start_upload(sess, ctx);
            } else {
                self.upload_error = Some("Sign in to Roblox first to upload assets.".into());
            }
        } else if want_browse {
            self.start_browse(session, ctx);
        }

        if let Some(uri) = commit_uri {
            self.close();
            return Some(PickerApply { target, uri });
        }

        if !open {
            self.close();
        }
        None
    }
}
