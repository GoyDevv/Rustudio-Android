//! Roblox Studio-style Asset ID Picker / Upload-to-Roblox / ID Browser dialog.
//!
//! Opened whenever the user clicks a `Variant::ContentId` or `Variant::Content`
//! property in the Properties panel (such as `Image`, `Texture`, `TextureID`,
//! `HoverImage`, `PressedImage`, `SoundId`, `AudioContent`, `MeshId`,
//! `MeshContent`, `AnimationId`, `ShirtTemplate`, `PantsTemplate`, `Skybox*`).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use egui::{Color32, RichText};
use rbx_dom_weak::types::Ref;
use rustudio_account::cloud::{CloudApi, Endpoints};
use rustudio_account::session::Session;

#[cfg(target_os = "android")]
use crate::file_dialog::Kind as FileDialogKind;
use crate::file_dialog::Request as FileDialogRequest;
use crate::toolbox::api::{self as tb_api, AnonHttp, Failure, Filters, ListReq, SessionHttp, Source};
use crate::toolbox::categories::Kind as ToolboxKind;
use crate::toolbox::model as tb_model;

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

/// Compact row displayed in the Browse Roblox tab.
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

/// Modal state for the Roblox Studio-style Asset Picker.
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

    // Tab 2: Upload to Roblox
    selected_file: Option<PathBuf>,
    asset_name: String,
    uploading: bool,
    upload_rx: Option<Receiver<Result<u64, String>>>,
    upload_error: Option<String>,

    // Native / Android file picker owned by the Asset Picker
    #[cfg(target_os = "android")]
    file_picker: Option<crate::android::storage::Picker>,
    #[cfg(not(target_os = "android"))]
    file_picker_rx: Option<Receiver<Option<PathBuf>>>,

    // Tab 3: Browse Roblox (ID Browser)
    browse_source: BrowseSource,
    browse_kind: ToolboxKind,
    search_query: String,
    browse_loading: bool,
    browse_fetched_once: bool,
    browse_rx: Option<Receiver<Result<Vec<BrowseItem>, String>>>,
    browse_items: Vec<BrowseItem>,
    browse_error: Option<String>,
}

impl AssetPicker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when background upload or search work is in flight.
    pub fn busy(&self) -> bool {
        self.uploading || self.browse_loading
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
        let category = PickerCategory::infer(&prop_name, &class_name);
        self.open = true;
        self.category = category;
        self.tab = PickerTab::PasteId;
        self.current_uri = current_uri.clone();
        self.input_uri = current_uri;
        self.paste_notice = None;

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
        self.search_query.clear();
        self.browse_loading = false;
        self.browse_fetched_once = false;
        self.browse_rx = None;
        self.browse_items.clear();
        self.browse_error = None;

        self.target = Some(PickerTarget {
            targets,
            class_name,
            prop_name,
            is_content_variant,
        });
    }

    /// Closes the modal and clears transient worker receivers.
    pub fn close(&mut self) {
        self.open = false;
        self.uploading = false;
        self.upload_rx = None;
        self.browse_loading = false;
        self.browse_rx = None;
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

    /// Polls the file picker and updates `selected_file` / `asset_name` when chosen.
    /// Returns `true` if the Android in-app file picker is currently displayed on screen.
    fn poll_file_picker(&mut self, ctx: &egui::Context) -> bool {
        #[cfg(target_os = "android")]
        {
            if let Some(picker) = self.file_picker.as_mut() {
                if let Some(chosen) = picker.show(ctx) {
                    self.file_picker = None;
                    if let Some(path) = chosen {
                        self.set_selected_file(path);
                    }
                    return false;
                }
                return true;
            }
            false
        }

        #[cfg(not(target_os = "android"))]
        {
            let _ = ctx;
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
                    self.set_selected_file(path);
                }
            }
            false
        }
    }

    fn set_selected_file(&mut self, path: PathBuf) {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Asset")
            .to_string();
        self.asset_name = stem;
        self.selected_file = Some(path);
        self.upload_error = None;
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
    fn start_browse(&mut self, session: Option<Session>, ctx: &egui::Context) {
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
        if let Some(rx) = self.browse_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(items)) => {
                    self.browse_loading = false;
                    self.browse_rx = None;
                    self.browse_error = None;
                    self.browse_items = items;
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
        let max_w = (screen.width() - 16.0).max(260.0);
        let max_h = (screen.height() - 16.0).max(240.0);
        let win_w = 520.0_f32.min(max_w);
        let win_h = 460.0_f32.min(max_h);

        let mut open = self.open;
        let mut commit_uri: Option<String> = None;
        let mut want_file_picker = false;
        let mut want_upload = false;
        let mut want_browse = false;

        egui::Window::new(format!("Asset Picker - {}", target.prop_name))
            .id(egui::Id::new("rustudio_asset_picker_window"))
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .default_size(egui::vec2(win_w, win_h))
            .max_width(max_w)
            .max_height(max_h)
            .constrain_to(screen.shrink(8.0))
            .collapsible(false)
            .resizable(true)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.spacing_mut().button_padding = egui::vec2(10.0, 6.0);

                // Tab bar
                ui.horizontal_wrapped(|ui| {
                    let prev_tab = self.tab;
                    ui.selectable_value(&mut self.tab, PickerTab::PasteId, "Paste / ID");
                    ui.selectable_value(&mut self.tab, PickerTab::Upload, "Upload to Roblox");
                    ui.selectable_value(&mut self.tab, PickerTab::Browse, "Browse Roblox");
                    if self.tab == PickerTab::Browse && prev_tab != PickerTab::Browse && !self.browse_fetched_once {
                        want_browse = true;
                    }
                });
                ui.separator();

                match self.tab {
                    PickerTab::PasteId => {
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
                        ui.add_space(8.0);

                        ui.label("Asset ID or URI (numbers auto-format to rbxassetid://...):");
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.input_uri)
                                .desired_width(f32::INFINITY)
                                .hint_text("123456789 or rbxassetid://123456789"),
                        );
                        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            commit_uri = Some(normalize_asset_uri(&self.input_uri));
                        }

                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Paste").clicked() {
                                if let Some(clip) = read_system_clipboard() {
                                    self.input_uri = normalize_asset_uri(&clip);
                                    self.paste_notice = None;
                                } else {
                                    self.paste_notice = Some("Clipboard is empty (or paste directly into the box).".into());
                                }
                            }
                            if ui.button("Clear").clicked() {
                                self.input_uri.clear();
                                self.paste_notice = None;
                            }
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Apply").strong().color(Color32::WHITE))
                                        .fill(Color32::from_rgb(38, 108, 224)),
                                )
                                .clicked()
                            {
                                commit_uri = Some(normalize_asset_uri(&self.input_uri));
                            }
                        });

                        let preview = normalize_asset_uri(&self.input_uri);
                        if !preview.is_empty() && preview != self.input_uri.trim() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(format!("Will apply: {preview}")).small().weak());
                        }
                        if let Some(msg) = &self.paste_notice {
                            ui.add_space(6.0);
                            ui.label(RichText::new(msg).small().color(Color32::from_rgb(230, 185, 80)));
                        }
                    }

                    PickerTab::Upload => {
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
                        ui.add_space(8.0);

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
                            if let Some(path) = &self.selected_file {
                                let fname = path
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("selected file");
                                ui.label(RichText::new(fname).monospace());
                            } else {
                                ui.label(RichText::new("No file selected").weak());
                            }
                        });

                        ui.add_space(8.0);
                        ui.label("Asset Name:");
                        ui.add_enabled(
                            !self.uploading,
                            egui::TextEdit::singleline(&mut self.asset_name)
                                .desired_width(f32::INFINITY)
                                .hint_text("My Asset"),
                        );

                        ui.add_space(10.0);
                        let can_upload = session.is_some() && self.selected_file.is_some() && !self.uploading;
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    can_upload,
                                    egui::Button::new(RichText::new("Upload & Apply").strong().color(Color32::WHITE))
                                        .fill(Color32::from_rgb(38, 108, 224)),
                                )
                                .clicked()
                            {
                                want_upload = true;
                            }
                            if self.uploading {
                                ui.spinner();
                                ui.label("Uploading to Roblox...");
                            }
                        });

                        if let Some(err) = &self.upload_error {
                            ui.add_space(8.0);
                            ui.label(RichText::new(err).color(Color32::from_rgb(235, 95, 95)));
                        }
                    }

                    PickerTab::Browse => {
                        // Source toggle (Creator Store vs My Creations) + Asset type filter
                        ui.horizontal_wrapped(|ui| {
                            let prev_src = self.browse_source;
                            ui.selectable_value(&mut self.browse_source, BrowseSource::CreatorStore, "Creator Store");
                            ui.selectable_value(&mut self.browse_source, BrowseSource::MyCreations, "My Creations");
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
                                    ui.selectable_value(&mut self.browse_kind, ToolboxKind::Decal, "Images / Decals");
                                    ui.selectable_value(&mut self.browse_kind, ToolboxKind::Audio, "Audio");
                                    ui.selectable_value(&mut self.browse_kind, ToolboxKind::MeshPart, "Meshes");
                                    ui.selectable_value(&mut self.browse_kind, ToolboxKind::Animation, "Animations");
                                    ui.selectable_value(&mut self.browse_kind, ToolboxKind::Model, "Models");
                                });
                            if self.browse_kind != prev_kind {
                                want_browse = true;
                            }
                        });

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .desired_width((ui.available_width() - 84.0).max(120.0))
                                    .hint_text("Search Roblox assets..."),
                            );
                            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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

                        let list_h = (ui.available_height() - 8.0).max(140.0);
                        egui::ScrollArea::vertical()
                            .id_salt("asset_picker_browse_scroll")
                            .max_height(list_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if !self.browse_loading && self.browse_items.is_empty() && self.browse_error.is_none() {
                                    ui.label(RichText::new("No matching assets found.").weak());
                                }
                                for item in &self.browse_items {
                                    let row_frame = egui::Frame::new()
                                        .fill(Color32::from_rgb(38, 40, 48))
                                        .corner_radius(4.0)
                                        .inner_margin(egui::Margin::symmetric(8, 6));
                                    let inner = row_frame.show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.vertical(|ui| {
                                                ui.set_max_width((ui.available_width() - 68.0).max(100.0));
                                                ui.label(RichText::new(&item.name).strong());
                                                let meta = if item.creator.is_empty() {
                                                    format!("ID: {}", item.id)
                                                } else {
                                                    format!("by {}  •  ID: {}", item.creator, item.id)
                                                };
                                                ui.label(RichText::new(meta).small().weak());
                                            });
                                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("Use").strong().color(Color32::WHITE),
                                                        )
                                                        .fill(Color32::from_rgb(38, 108, 224)),
                                                    )
                                                    .clicked()
                                                {
                                                    commit_uri = Some(format!("rbxassetid://{}", item.id));
                                                }
                                            });
                                        });
                                    });
                                    // Tapping anywhere on the row also applies the asset immediately.
                                    let row_click = ui.interact(
                                        inner.response.rect,
                                        ui.id().with(("asset_picker_row", item.id)),
                                        egui::Sense::click(),
                                    );
                                    if row_click.clicked() {
                                        commit_uri = Some(format!("rbxassetid://{}", item.id));
                                    }
                                    ui.add_space(4.0);
                                }
                            });
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
