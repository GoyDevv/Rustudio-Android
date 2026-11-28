//! Roblox Studio–parity unified asset import system (pure Rust, zero C/C++ crates):
//! - Phase 1: Animation import (FBX / glTF / GLB / RBXM / RBXMX) with Mixamo & Blender -> R15/R6 bone remapper
//! - Phase 2: Local Image (PNG/JPG/WEBP/BMP/TGA), Audio (MP3/OGG/WAV/FLAC), and Script (.lua/.luau/.txt) import
//! - Phase 3: PBR `SurfaceAppearance` extraction & mobile-clamped 3D Import Settings modal
//! - Phase 4: Unified Ribbon / File menu dialogs + Android SAF compatibility

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use egui::RichText;
use glam::{Mat3, Quat, Vec3};
use rbx_dom_weak::types::{CFrame, Content, Enum, Matrix3, Ref, UDim2, Variant, Vector3};
use rbx_dom_weak::InstanceBuilder;

use crate::app::Editor;
use crate::file_dialog::{Kind, Purpose, Request};

static ASSET_IMPORT_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    ASSET_IMPORT_SEQ.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RigKind {
    R15,
    R6,
}

#[derive(Clone, Debug)]
pub struct Import3DSettings {
    pub scale: f32,
    pub anchored: bool,
    pub import_pbr: bool,
    pub insert_at_camera: bool,
    pub target_rig: RigKind,
    /// When `Some`, the 3D Import Settings modal is open for this file.
    pub pending_path: Option<PathBuf>,
}

impl Default for Import3DSettings {
    fn default() -> Self {
        Self {
            scale: 1.0,
            anchored: true,
            import_pbr: true,
            insert_at_camera: true,
            target_rig: RigKind::R15,
            pending_path: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 1: Mixamo / Blender -> Roblox R15 / R6 Bone Remapper & Animation Import
// ---------------------------------------------------------------------------

/// Maps Mixamo (`mixamorig:Hips`, `mixamorig:LeftArm`, ...), Blender Rigify
/// (`DEF-hips`, `thigh.L`, `upper_arm.R`, ...), and standard Roblox bone names
/// to canonical Roblox R15 or R6 joint names.
pub fn remap_bone_name(raw: &str, rig: RigKind) -> Option<&'static str> {
    let mut s = raw.trim();
    if let Some((_, tail)) = s.rsplit_once(':') {
        s = tail;
    }
    for prefix in ["DEF-", "ORG-", "MCH-", "mixamorig_", "mixamorig", "ValveBiped.Bip01_", "Bip01_"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    let norm: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase();

    let r15_name: &'static str = match norm.as_str() {
        // Root / Pelvis / Hips
        "humanoidrootpart" | "root" | "rootpart" => "HumanoidRootPart",
        "lowertorso" | "hips" | "pelvis" | "spine" => "LowerTorso",
        "uppertorso" | "spine1" | "spine2" | "spine.002" | "spine.003" | "chest" | "torso" => "UpperTorso",
        "head" | "neck" | "spine.006" => "Head",

        // Left Arm
        "leftupperarm" | "leftarm" | "left_arm" | "upper_arm.l" | "upperarm.l" | "arm_l" | "l_upperarm" => "LeftUpperArm",
        "leftlowerarm" | "leftforearm" | "left_forearm" | "forearm.l" | "lowerarm.l" | "l_forearm" => "LeftLowerArm",
        "lefthand" | "left_hand" | "hand.l" | "l_hand" => "LeftHand",

        // Right Arm
        "rightupperarm" | "rightarm" | "right_arm" | "upper_arm.r" | "upperarm.r" | "arm_r" | "r_upperarm" => "RightUpperArm",
        "rightlowerarm" | "rightforearm" | "right_forearm" | "forearm.r" | "lowerarm.r" | "r_forearm" => "RightLowerArm",
        "righthand" | "right_hand" | "hand.r" | "r_hand" => "RightHand",

        // Left Leg
        "leftupperleg" | "leftupleg" | "left_upleg" | "thigh.l" | "upperleg.l" | "l_thigh" => "LeftUpperLeg",
        "leftlowerleg" | "leftleg" | "left_leg" | "shin.l" | "calf.l" | "lowerleg.l" | "l_calf" => "LeftLowerLeg",
        "leftfoot" | "left_foot" | "foot.l" | "l_foot" => "LeftFoot",

        // Right Leg
        "rightupperleg" | "rightupleg" | "right_upleg" | "thigh.r" | "upperleg.r" | "r_thigh" => "RightUpperLeg",
        "rightlowerleg" | "rightleg" | "right_leg" | "shin.r" | "calf.r" | "lowerleg.r" | "r_calf" => "RightLowerLeg",
        "rightfoot" | "right_foot" | "foot.r" | "r_foot" => "RightFoot",

        _ => return None,
    };

    match rig {
        RigKind::R15 => Some(r15_name),
        RigKind::R6 => Some(match r15_name {
            "HumanoidRootPart" => "HumanoidRootPart",
            "LowerTorso" | "UpperTorso" => "Torso",
            "Head" => "Head",
            "LeftUpperArm" | "LeftLowerArm" | "LeftHand" => "Left Arm",
            "RightUpperArm" | "RightLowerArm" | "RightHand" => "Right Arm",
            "LeftUpperLeg" | "LeftLowerLeg" | "LeftFoot" => "Left Leg",
            "RightUpperLeg" | "RightLowerLeg" | "RightFoot" => "Right Leg",
            _ => "Torso",
        }),
    }
}

fn cframe_from_pos_quat(pos: Vec3, q: Quat) -> CFrame {
    let m = Mat3::from_quat(q.normalize());
    let cols = m.to_cols_array();
    // glam Mat3 is column-major: [x0, x1, x2, y0, y1, y2, z0, z1, z2]
    let rot = Matrix3::new(
        Vector3::new(cols[0], cols[3], cols[6]),
        Vector3::new(cols[1], cols[4], cols[7]),
        Vector3::new(cols[2], cols[5], cols[8]),
    );
    CFrame::new(Vector3::new(pos.x, pos.y, pos.z), rot)
}

struct SimpleKeyframe {
    time: f32,
    poses: HashMap<String, CFrame>,
}

/// Minimal pure-Rust glTF/GLB animation parser extracting translation/rotation tracks.
fn parse_gltf_animation(bytes: &[u8], rig: RigKind) -> Result<(String, Vec<SimpleKeyframe>), String> {
    let (json, bin_chunk): (serde_json::Value, Option<&[u8]>) = if bytes.len() >= 20 && &bytes[0..4] == b"glTF" {
        let mut off = 12usize;
        let mut json_slice: Option<&[u8]> = None;
        let mut bin_slice: Option<&[u8]> = None;
        while off + 8 <= bytes.len() {
            let chunk_len = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
            let chunk_type = &bytes[off + 4..off + 8];
            off += 8;
            if off + chunk_len > bytes.len() {
                break;
            }
            if chunk_type == b"JSON" {
                json_slice = Some(&bytes[off..off + chunk_len]);
            } else if chunk_type == b"BIN\0" {
                bin_slice = Some(&bytes[off..off + chunk_len]);
            }
            off += chunk_len;
        }
        let js = json_slice.ok_or_else(|| "GLB missing JSON chunk".to_string())?;
        let val: serde_json::Value = serde_json::from_slice(js).map_err(|e| format!("GLB JSON: {e}"))?;
        (val, bin_slice)
    } else {
        let val: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("glTF JSON: {e}"))?;
        (val, None)
    };

    let anims = json
        .get("animations")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| "glTF file contains no animations".to_string())?;
    let anim = &anims[0];
    let anim_name = anim
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("ImportedAnimation")
        .to_string();

    let accessors = json.get("accessors").and_then(|v| v.as_array()).ok_or("glTF missing accessors")?;
    let buffer_views = json.get("bufferViews").and_then(|v| v.as_array()).ok_or("glTF missing bufferViews")?;
    let nodes = json.get("nodes").and_then(|v| v.as_array()).ok_or("glTF missing nodes")?;

    let read_f32s = |acc_idx: usize| -> Option<Vec<f32>> {
        let acc = accessors.get(acc_idx)?;
        if acc.get("componentType")?.as_u64()? != 5126 {
            return None; // FLOAT only
        }
        let count = acc.get("count")?.as_u64()? as usize;
        let comps = match acc.get("type")?.as_str()? {
            "SCALAR" => 1usize,
            "VEC3" => 3usize,
            "VEC4" => 4usize,
            _ => return None,
        };
        let bv_idx = acc.get("bufferView")?.as_u64()? as usize;
        let bv = buffer_views.get(bv_idx)?;
        let bin = bin_chunk?;
        let base = bv.get("byteOffset").and_then(|v| v.as_u64()).unwrap_or(0) as usize
            + acc.get("byteOffset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let stride = bv.get("byteStride").and_then(|v| v.as_u64()).unwrap_or((comps * 4) as u64) as usize;
        let max_items = bin.len().saturating_sub(base) / (comps * 4).max(1);
    let mut out = Vec::with_capacity(count.min(max_items).saturating_mul(comps));
        for i in 0..count {
            let start = base + i * stride;
            if start + comps * 4 > bin.len() {
                break;
            }
            for c in 0..comps {
                let b = start + c * 4;
                out.push(f32::from_le_bytes(bin[b..b + 4].try_into().ok()?));
            }
        }
        Some(out)
    };

    let samplers = anim.get("samplers").and_then(|v| v.as_array()).ok_or("glTF animation missing samplers")?;
    let channels = anim.get("channels").and_then(|v| v.as_array()).ok_or("glTF animation missing channels")?;

    // Collect per-time-bucket (quantized to 1ms) joint poses
    let mut buckets: std::collections::BTreeMap<i64, HashMap<String, (Vec3, Quat)>> = std::collections::BTreeMap::new();

    for ch in channels {
        let Some(sampler_idx) = ch.get("sampler").and_then(|v| v.as_u64()).map(|v| v as usize) else { continue };
        let Some(target) = ch.get("target") else { continue };
        let Some(node_idx) = target.get("node").and_then(|v| v.as_u64()).map(|v| v as usize) else { continue };
        let Some(path) = target.get("path").and_then(|v| v.as_str()) else { continue };
        if path != "translation" && path != "rotation" {
            continue;
        }
        let raw_bone = nodes
            .get(node_idx)
            .and_then(|n| n.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let bone_name = remap_bone_name(raw_bone, rig)
            .map(|s| s.to_string())
            .unwrap_or_else(|| if raw_bone.is_empty() { format!("Joint{node_idx}") } else { raw_bone.to_string() });

        let Some(sampler) = samplers.get(sampler_idx) else { continue };
        let Some(in_idx) = sampler.get("input").and_then(|v| v.as_u64()).map(|v| v as usize) else { continue };
        let Some(out_idx) = sampler.get("output").and_then(|v| v.as_u64()).map(|v| v as usize) else { continue };
        let (Some(times), Some(vals)) = (read_f32s(in_idx), read_f32s(out_idx)) else { continue };

        for (i, &t) in times.iter().enumerate() {
            if !t.is_finite() || t < 0.0 {
                continue;
            }
            let key = (t * 1000.0).round() as i64;
            let entry = buckets
                .entry(key)
                .or_default()
                .entry(bone_name.clone())
                .or_insert((Vec3::ZERO, Quat::IDENTITY));
            if path == "translation" && i * 3 + 2 < vals.len() {
                entry.0 = Vec3::new(vals[i * 3], vals[i * 3 + 1], vals[i * 3 + 2]);
            } else if path == "rotation" && i * 4 + 3 < vals.len() {
                entry.1 = Quat::from_xyzw(vals[i * 4], vals[i * 4 + 1], vals[i * 4 + 2], vals[i * 4 + 3]).normalize();
            }
        }
    }

    if buckets.is_empty() {
        return Err("No keyframe tracks found in glTF animation".to_string());
    }
    let frames = buckets
        .into_iter()
        .map(|(ms, map)| SimpleKeyframe {
            time: ms as f32 / 1000.0,
            poses: map
                .into_iter()
                .map(|(k, (p, q))| (k, cframe_from_pos_quat(p, q)))
                .collect(),
        })
        .collect();
    Ok((anim_name, frames))
}

// ---------------------------------------------------------------------------
// Phase 3: PBR Roughness/Metalness Channel Splitter (glTF G=Rough, B=Metal)
// ---------------------------------------------------------------------------

/// Splits a glTF `metallicRoughnessTexture` (G = roughness, B = metalness) into
/// separate grayscale PNG buffers for Roblox `SurfaceAppearance.RoughnessMap`
/// and `SurfaceAppearance.MetalnessMap`.
pub fn split_gltf_metallic_roughness(bytes: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut rough = image::RgbaImage::new(w, h);
    let mut metal = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels() {
        let g = px[1];
        let b = px[2];
        rough.put_pixel(x, y, image::Rgba([g, g, g, 255]));
        metal.put_pixel(x, y, image::Rgba([b, b, b, 255]));
    }
    let mut rough_png = Vec::new();
    let mut metal_png = Vec::new();
    image::DynamicImage::ImageRgba8(rough)
        .write_to(&mut std::io::Cursor::new(&mut rough_png), image::ImageFormat::Png)
        .ok()?;
    image::DynamicImage::ImageRgba8(metal)
        .write_to(&mut std::io::Cursor::new(&mut metal_png), image::ImageFormat::Png)
        .ok()?;
    Some((rough_png, metal_png))
}

/// Ensures image bytes (including WebP / BMP / TGA) are stored as PNG if needed
/// so Roblox's texture pipeline decodes every format reliably.
pub fn normalize_image_bytes(path: &Path, bytes: Vec<u8>) -> Result<((u32, u32), Vec<u8>), String> {
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("Could not decode image {}: {e}", path.display()))?;
    let dims = (img.width(), img.height());
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
        return Ok((dims, bytes));
    }
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("PNG conversion failed: {e}"))?;
    Ok((dims, png))
}

// ---------------------------------------------------------------------------
// Editor Methods (Dialogs, Importers, 3D Import Settings Modal)
// ---------------------------------------------------------------------------

impl Editor {
    pub fn import_animation_dialog(&mut self) {
        let d = Request::new()
            .set_title("Import Animation")
            .add_filter("Animations", &["fbx", "glb", "gltf", "rbxm", "rbxmx"]);
        self.dialogs.start(Purpose::ImportAnimation, d, Kind::PickFile);
    }

    pub fn import_image_dialog(&mut self) {
        let d = Request::new()
            .set_title("Import Image / Decal")
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "tga"]);
        self.dialogs.start(Purpose::ImportImage, d, Kind::PickFile);
    }

    pub fn import_audio_dialog(&mut self) {
        let d = Request::new()
            .set_title("Import Audio")
            .add_filter("Audio", &["mp3", "ogg", "wav", "flac"]);
        self.dialogs.start(Purpose::ImportAudio, d, Kind::PickFile);
    }

    pub fn import_script_dialog(&mut self) {
        let d = Request::new()
            .set_title("Import Script")
            .add_filter("Luau Scripts", &["luau", "lua", "txt"]);
        self.dialogs.start(Purpose::ImportScript, d, Kind::PickFile);
    }

    /// Phase 1: Imports an animation file (FBX / glTF / GLB / RBXM / RBXMX) into
    /// a Roblox `KeyframeSequence` inside the selected rig's `AnimSaves` or `ReplicatedStorage`.
    pub fn import_animation_file(&mut self, path: PathBuf) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.error(format!("Could not read {}: {e}", path.display()));
                return;
            }
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();

        // Target parent: if a Model with a Humanoid is selected, put into its AnimSaves; else ReplicatedStorage.
        let root = self.game.doc.dom.root_ref();
        let selected_rig = self.game.doc.selection.first().copied().filter(|&r| {
            self.game.doc.dom.get_by_ref(r).is_some_and(|i| i.class == "Model")
        });
        let parent = if let Some(rig_ref) = selected_rig {
            if let Some(existing) = self.game.doc.find_child(rig_ref, "AnimSaves") {
                existing
            } else {
                self.game.doc.insert(rig_ref, InstanceBuilder::new("Model").with_name("AnimSaves"))
            }
        } else {
            self.game
                .doc
                .find_child(root, "ReplicatedStorage")
                .unwrap_or_else(|| self.game.doc.default_parent())
        };

        if matches!(ext.as_str(), "rbxm" | "rbxmx") {
            let refs = self.game.doc.paste(&bytes, parent, "Import Animation");
            if refs.is_empty() {
                self.error(format!("Could not decode animation model {}", path.display()));
            } else {
                self.explorer.expanded.insert(parent);
                self.select(refs);
                self.info(format!("Imported animation from {}", path.display()));
            }
            return;
        }

        let rig = self.import_3d_settings.target_rig;
        let r15 = rig == RigKind::R15;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("ImportedAnimation")
            .to_string();

        if ext == "fbx" || rustudio_core::fbx::is_fbx(&bytes) {
            match rustudio_core::fbx_anim::read(&bytes, r15) {
                Ok(clip) => {
                    let kfs = self.game.doc.insert(
                        parent,
                        InstanceBuilder::new("KeyframeSequence")
                            .with_name(if clip.name.is_empty() { stem } else { clip.name.clone() })
                            .with_property("Loop", true)
                            .with_property("Priority", Variant::Enum(Enum::from_u32(2))),
                    );
                    for (fi, &time) in clip.times.iter().enumerate() {
                        let kf_ref = self.game.doc.insert(
                            kfs,
                            InstanceBuilder::new("Keyframe")
                                .with_name(if fi + 1 == clip.times.len() { "End" } else { "Keyframe" })
                                .with_property("Time", time as f32),
                        );
                        let mut made_poses: Vec<Ref> = Vec::with_capacity(clip.bones.len());
                        for bone in &clip.bones {
                            let parent_ref = bone
                                .parent
                                .and_then(|pi| made_poses.get(pi).copied())
                                .unwrap_or(kf_ref);
                            let mapped = remap_bone_name(&bone.name, rig).unwrap_or(&bone.name);
                            let cf = bone.poses.get(fi).map_or_else(
                                || CFrame::new(Vector3::new(0.0, 0.0, 0.0), Matrix3::identity()),
                                |xf| {
                                    rustudio_core::math::Xform {
                                        pos: xf.pos.as_vec3(),
                                        rot: xf.rot.as_mat3(),
                                    }
                                    .to_cframe()
                                },
                            );
                            let pose_ref = self.game.doc.insert(
                                parent_ref,
                                InstanceBuilder::new("Pose")
                                    .with_name(mapped)
                                    .with_property("CFrame", cf)
                                    .with_property("Weight", 1.0f32),
                            );
                            made_poses.push(pose_ref);
                        }
                    }
                    self.explorer.expanded.insert(parent);
                    self.explorer.expanded.insert(kfs);
                    self.select(vec![kfs]);
                    self.info(format!(
                        "Imported FBX animation ({} keyframes) from {}",
                        clip.times.len(),
                        path.display()
                    ));
                }
                Err(e) => self.error(format!("Could not import FBX animation {}: {e}", path.display())),
            }
            return;
        }

        if matches!(ext.as_str(), "glb" | "gltf") {
            match parse_gltf_animation(&bytes, rig) {
                Ok((name, frames)) => {
                    let kfs = self.game.doc.insert(
                        parent,
                        InstanceBuilder::new("KeyframeSequence")
                            .with_name(if name.is_empty() { stem } else { name })
                            .with_property("Loop", true)
                            .with_property("Priority", Variant::Enum(Enum::from_u32(2))),
                    );
                    for kf in &frames {
                        let kf_ref = self.game.doc.insert(
                            kfs,
                            InstanceBuilder::new("Keyframe")
                                .with_name("Keyframe")
                                .with_property("Time", kf.time),
                        );
                        let root_pose = self.game.doc.insert(
                            kf_ref,
                            InstanceBuilder::new("Pose")
                                .with_name("HumanoidRootPart")
                                .with_property("Weight", 1.0f32),
                        );
                        for (joint, cf) in &kf.poses {
                            let _ = self.game.doc.insert(
                                root_pose,
                                InstanceBuilder::new("Pose")
                                    .with_name(joint.clone())
                                    .with_property("CFrame", *cf)
                                    .with_property("Weight", 1.0f32),
                            );
                        }
                    }
                    self.explorer.expanded.insert(parent);
                    self.explorer.expanded.insert(kfs);
                    self.select(vec![kfs]);
                    self.info(format!(
                        "Imported glTF animation ({} keyframes) from {}",
                        frames.len(),
                        path.display()
                    ));
                }
                Err(e) => self.error(format!("Could not import glTF animation {}: {e}", path.display())),
            }
            return;
        }

        self.error(format!("Unsupported animation format: {}", path.display()));
    }

    /// Phase 2A: Imports a local image (PNG/JPG/WEBP/BMP/TGA) as an `ImageLabel`
    /// (if a GUI container is selected), a `Decal` on a selected `BasePart`, or
    /// a new sized `Part` + `Decal` in front of the camera.
    pub fn import_image_file(&mut self, path: PathBuf) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.error(format!("Could not read {}: {e}", path.display()));
                return;
            }
        };
        let ((w, h), normalized) = match normalize_image_bytes(&path, bytes) {
            Ok(v) => v,
            Err(e) => {
                self.error(e);
                return;
            }
        };
        let seq = next_seq();
        let tex_id = format!("rbxtemp://rustudio/import/image/{seq}");
        rustudio_core::assets::register_generated(&tex_id, normalized);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("ImportedImage")
            .to_string();

        let sel = self.game.doc.selection.first().copied();
        let sel_class = sel
            .and_then(|r| self.game.doc.dom.get_by_ref(r))
            .map(|i| i.class.as_str())
            .unwrap_or("");

        if matches!(
            sel_class,
            "ScreenGui" | "SurfaceGui" | "BillboardGui" | "Frame" | "ScrollingFrame" | "CanvasGroup"
        ) {
            let parent = sel.unwrap();
            let r = self.game.doc.insert(
                parent,
                InstanceBuilder::new("ImageLabel")
                    .with_name(stem)
                    .with_property("Image", Variant::ContentId(tex_id.clone().into()))
                    .with_property("ImageContent", Variant::Content(Content::from_uri(tex_id)))
                    .with_property("Size", Variant::UDim2(UDim2::new(
                        rbx_dom_weak::types::UDim::new(0.0, w.min(512) as i32),
                        rbx_dom_weak::types::UDim::new(0.0, h.min(512) as i32),
                    ))),
            );
            self.explorer.expanded.insert(parent);
            self.select(vec![r]);
            self.info(format!("Imported image ({w}×{h}) as ImageLabel from {}", path.display()));
            return;
        }

        if let Some(part_ref) = sel.filter(|_| rustudio_core::doc::is_a(sel_class, "BasePart")) {
            let r = self.game.doc.insert(
                part_ref,
                InstanceBuilder::new("Decal")
                    .with_name(stem)
                    .with_property("Texture", Variant::ContentId(tex_id.clone().into()))
                    .with_property("ColorMapContent", Variant::Content(Content::from_uri(tex_id))),
            );
            self.explorer.expanded.insert(part_ref);
            self.select(vec![r]);
            self.info(format!("Applied image ({w}×{h}) as Decal from {}", path.display()));
            return;
        }

        let aspect = (w.max(1) as f32) / (h.max(1) as f32);
        let part_h = 6.0f32;
        let part_w = (part_h * aspect).clamp(1.0, 24.0);
        let pos = self.game.cam.pos + self.game.cam.forward() * 14.0;
        let pos = Vec3::new(pos.x, pos.y.max(part_h * 0.5 + 0.5), pos.z);
        let parent = self.game.doc.default_parent();
        let part = self.game.doc.insert(
            parent,
            InstanceBuilder::new("Part")
                .with_name(stem.clone())
                .with_property("Size", Vector3::new(part_w, part_h, 0.2))
                .with_property("CFrame", CFrame::new(Vector3::new(pos.x, pos.y, pos.z), Matrix3::identity()))
                .with_property("Anchored", true),
        );
        let _ = self.game.doc.insert(
            part,
            InstanceBuilder::new("Decal")
                .with_name(stem)
                .with_property("Texture", Variant::ContentId(tex_id.clone().into()))
                .with_property("ColorMapContent", Variant::Content(Content::from_uri(tex_id))),
        );
        self.explorer.expanded.insert(parent);
        self.explorer.expanded.insert(part);
        self.select(vec![part]);
        self.info(format!("Imported image ({w}×{h}) from {}", path.display()));
    }

    /// Phase 2B: Imports a local audio file (MP3/OGG/WAV/FLAC) as a `Sound` instance.
    pub fn import_audio_file(&mut self, path: PathBuf) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.error(format!("Could not read {}: {e}", path.display()));
                return;
            }
        };
        if bytes.is_empty() {
            self.error(format!("Audio file {} is empty", path.display()));
            return;
        }
        let seq = next_seq();
        let sound_id = format!("rbxtemp://rustudio/import/audio/{seq}");
        rustudio_core::assets::register_generated(&sound_id, bytes);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("ImportedSound")
            .to_string();
        let root = self.game.doc.dom.root_ref();
        let parent = self
            .game
            .doc
            .selection
            .first()
            .copied()
            .filter(|&r| self.game.doc.exists(r) && r != root)
            .or_else(|| self.game.doc.find_child(root, "SoundService"))
            .unwrap_or_else(|| self.game.doc.default_parent());
        let r = self.game.doc.insert(
            parent,
            InstanceBuilder::new("Sound")
                .with_name(stem)
                .with_property("SoundId", Variant::ContentId(sound_id.clone().into()))
                .with_property("AudioContent", Variant::Content(Content::from_uri(sound_id)))
                .with_property("Volume", 0.5f32),
        );
        self.explorer.expanded.insert(parent);
        self.select(vec![r]);
        self.info(format!("Imported audio from {}", path.display()));
    }

    /// Phase 2C: Imports a local `.lua` / `.luau` / `.txt` file as a `Script`,
    /// `LocalScript` (`.client.luau`), or `ModuleScript` (`.module.luau`).
    pub fn import_script_file(&mut self, path: PathBuf) {
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.error(format!("Could not read {}: {e}", path.display()));
                return;
            }
        };
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("ImportedScript.luau");
        let lower = fname.to_ascii_lowercase();
        let (class, default_service, strip_suffix) = if lower.ends_with(".client.luau") || lower.ends_with(".client.lua") {
            ("LocalScript", "StarterPlayer", ".client")
        } else if lower.ends_with(".module.luau") || lower.ends_with(".module.lua") {
            ("ModuleScript", "ReplicatedStorage", ".module")
        } else {
            ("Script", "ServerScriptService", ".server")
        };
        let mut name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("ImportedScript")
            .to_string();
        if let Some(stripped) = name.strip_suffix(strip_suffix) {
            if !stripped.is_empty() {
                name = stripped.to_string();
            }
        }
        let root = self.game.doc.dom.root_ref();
        let parent = self
            .game
            .doc
            .selection
            .first()
            .copied()
            .filter(|&r| self.game.doc.exists(r) && r != root)
            .or_else(|| self.game.doc.find_child(root, default_service))
            .unwrap_or_else(|| self.game.doc.default_parent());
        let r = self.game.doc.insert(
            parent,
            InstanceBuilder::new(class)
                .with_name(name)
                .with_property("Source", source),
        );
        self.explorer.expanded.insert(parent);
        self.select(vec![r]);
        self.info(format!("Imported {class} from {}", path.display()));
    }

    /// Phase 3: Mobile-clamped 3D Import Settings modal (scale, anchored, PBR SurfaceAppearance, placement).
    pub fn import_3d_settings_modal(&mut self, ctx: &egui::Context) {
        let Some(path) = self.import_3d_settings.pending_path.clone() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        let screen = ctx.content_rect();
        let max_w = (screen.width() - 32.0).clamp(260.0, 440.0);
        let max_h = (screen.height() - 110.0).max(160.0);

        egui::Modal::new(egui::Id::new("import_3d_settings_modal")).show(ctx, |ui| {
            ui.set_max_width(max_w);
            egui::ScrollArea::vertical()
                .id_salt("import_3d_settings_scroll")
                .max_height(max_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(RichText::new("Import 3D Settings").strong().size(15.0));
                    ui.add_space(4.0);
                    let fname = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    ui.label(RichText::new(fname).color(rustudio_engine::theme::TEXT_DIM));
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("Scale:");
                        ui.add(
                            egui::DragValue::new(&mut self.import_3d_settings.scale)
                                .speed(0.05)
                                .range(0.001..=100.0)
                                .suffix("×"),
                        );
                        if ui.small_button("1×").clicked() {
                            self.import_3d_settings.scale = 1.0;
                        }
                        if ui.small_button("0.01×").on_hover_text("cm → studs").clicked() {
                            self.import_3d_settings.scale = 0.01;
                        }
                        if ui.small_button("3.57×").on_hover_text("meters → studs").clicked() {
                            self.import_3d_settings.scale = 3.5714;
                        }
                    });
                    ui.add_space(4.0);
                    ui.checkbox(&mut self.import_3d_settings.anchored, "Anchored");
                    ui.checkbox(
                        &mut self.import_3d_settings.import_pbr,
                        "Import PBR Materials (SurfaceAppearance)",
                    );
                    ui.checkbox(
                        &mut self.import_3d_settings.insert_at_camera,
                        "Place in front of camera",
                    );
                });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Import").clicked() {
                    confirm = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });

        if confirm {
            self.import_3d_settings.pending_path = None;
            self.import_3d_file(path);
        } else if cancel {
            self.import_3d_settings.pending_path = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_mixamo_and_blender_bones_to_r15_and_r6() {
        assert_eq!(remap_bone_name("mixamorig:Hips", RigKind::R15), Some("LowerTorso"));
        assert_eq!(remap_bone_name("mixamorig:LeftArm", RigKind::R15), Some("LeftUpperArm"));
        assert_eq!(remap_bone_name("mixamorig:RightForeArm", RigKind::R15), Some("RightLowerArm"));
        assert_eq!(remap_bone_name("DEF-thigh.L", RigKind::R15), Some("LeftUpperLeg"));
        assert_eq!(remap_bone_name("upper_arm.R", RigKind::R6), Some("Right Arm"));
        assert_eq!(remap_bone_name("mixamorig:Hips", RigKind::R6), Some("Torso"));
    }
}
