//! PluginMouse (hook point for edit-mode input). plugin:GetMouse
//! gives each plugin one PluginMouse (Name "Instance", Parent nil:
//! measured), recorded as `vm::UiKind::Mouse`. PluginMouse is a Mouse, so
//! its Hit / Target / UnitRay / Origin / X / Y / ViewSizeX / ViewSizeY read
//! the VM's `World::input` (runtime_ext::Inputs) as a Player's Mouse does in
//! Play: edit-mode input fills `input` in the edit VM (through
//! `PluginHost::with_edit`) and fires Button1Down / Move / … on the active
//! plugin's mouse (`PluginHost::ui_items`, `active_plugin`). Until the
//! pointer is in the 3D view (`set_pointer_in_view`), X and Y read -1, as
//! measured for an idle PluginMouse in Studio (ViewSizeX 1736 there: the
//! viewport's size).

use mlua::{IntoLua, Lua, Value};
use rbx_dom_weak::types::Ref;

use super::*;

/// Builds a right-handed Roblox CFrame rotation whose LookVector (-Z) points
/// along `dir` (so `Mouse.Origin.LookVector` and `Mouse.Hit.LookVector` match
/// `Mouse.UnitRay.Direction`, as in Roblox Studio).
pub(crate) fn ray_look_rot(dir: glam::Vec3) -> glam::Mat3 {
    let fwd = dir.normalize_or_zero();
    if fwd.length_squared() < 1e-8 {
        return glam::Mat3::IDENTITY;
    }
    let up_ref = if fwd.y.abs() > 0.999 { glam::Vec3::Z } else { glam::Vec3::Y };
    let right = fwd.cross(up_ref).normalize_or_zero();
    let up = right.cross(fwd).normalize_or_zero();
    glam::Mat3::from_cols(right, up, -fwd)
}

/// Whether the pointer is over the 3D view (edit-mode input sets it).
#[derive(Default)]
struct Pointer(bool);

pub fn set_pointer_in_view(lua: &Lua, on: bool) {
    match lua.app_data_mut::<Pointer>() {
        Some(mut p) => p.0 = on,
        None => {
            lua.set_app_data(Pointer(on));
        }
    }
}

fn pointer_in_view(lua: &Lua) -> bool {
    lua.app_data_ref::<Pointer>().is_some_and(|p| p.0)
}

/// The plugin's PluginMouse (made on first use).
pub fn get_mouse(lua: &Lua, this: Ref) -> mlua::Result<Ref> {
    let root = plugin_root(lua, this)?;
    if let Some(m) = vm::vm(lua).and_then(|v| v.by_root(root).and_then(|p| p.mouse)) {
        return Ok(m);
    }
    let r = {
        let mut w = world(lua)?;
        w.dm()
            .dom
            .insert(Ref::none(), rbx_dom_weak::InstanceBuilder::new("PluginMouse").with_name("Instance"))
    };
    if let Some(mut v) = vm::vm(lua) {
        if let Some(p) = v.by_root_mut(root) {
            p.mouse = Some(r);
        }
    }
    vm::record_ui(lua, root, r, UiKind::Mouse);
    Ok(r)
}

pub fn prop_get(lua: &Lua, _r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    if class != "PluginMouse" {
        return Ok(None);
    }
    Ok(match key {
        "X" | "Y" if !pointer_in_view(lua) => Some(Value::Number(-1.0)),
        "TargetFilter" => {
            let tf = world(lua)?.input.mouse_target_filter;
            match tf {
                Some(r) if world(lua).is_ok_and(|w| w.d().exists(r)) => Some(inst_value(lua, r)?),
                _ => Some(Value::Nil),
            }
        }
        "UnitRay" => {
            let ray = world(lua)?.input.mouse_ray;
            let dir = ray.1.normalize_or_zero();
            Some(Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LRay {
                origin: ray.0,
                direction: dir,
            })?))
        }
        "Origin" => {
            let ray = world(lua)?.input.mouse_ray;
            let xf = rustudio_core::math::Xform {
                pos: ray.0,
                rot: ray_look_rot(ray.1),
            };
            Some(rustudio_engine::ltypes::LCFrame(xf).into_lua(lua)?)
        }
        "Hit" | "hit" => {
            let (ray, hit) = {
                let w = world(lua)?;
                (w.input.mouse_ray, w.input.mouse_hit)
            };
            let rot = ray_look_rot(ray.1);
            let pos = hit.map(|h| h.pos).unwrap_or_else(|| ray.0 + ray.1 * 1000.0);
            let xf = rustudio_core::math::Xform { pos, rot };
            Some(rustudio_engine::ltypes::LCFrame(xf).into_lua(lua)?)
        }
        _ => None,
    })
}
