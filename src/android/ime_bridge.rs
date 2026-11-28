//! Native Android `InputConnection` bridge from `RustudioActivity.java` directly
//! into RusTudio's real egui text fields (Script Editor, Properties, etc.) with
//! zero fake EditText overlay.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

use egui::{Color32, Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Shape, Stroke, TouchPhase};
use jni::objects::JValue;
use jni::{jni_sig, jni_str};

enum ImeOp {
    CommitText(String),
    Delete { before: i32, after: i32 },
    Key { code: i32, down: bool },
    HardwareKey { code: i32, meta: i32, down: bool, repeat: bool },
    Action(i32),
    HardwareMouseDetected(bool),
    PointerCaptureChanged(bool),
    MouseCapture {
        action: i32,
        dx: f32,
        dy: f32,
        button_state: i32,
        action_button: i32,
        scroll_x: f32,
        scroll_y: f32,
        abs_x: f32,
        abs_y: f32,
        is_captured: bool,
    },
}

struct VirtualMouseState {
    pos: Pos2,
    drag_pos: Pos2,
    initialized: bool,
    buttons: i32,
    active: bool,
    is_captured: bool,
    locked_look: bool,
    cursor_hidden: bool,
    right_press_pos: Option<Pos2>,
}

impl VirtualMouseState {
    const fn new() -> Self {
        Self {
            pos: Pos2::ZERO,
            drag_pos: Pos2::ZERO,
            initialized: false,
            buttons: 0,
            active: false,
            is_captured: false,
            locked_look: false,
            cursor_hidden: false,
            right_press_pos: None,
        }
    }
}

static VIRTUAL_MOUSE: Mutex<VirtualMouseState> = Mutex::new(VirtualMouseState::new());
static CURSOR_PLUGIN_INSTALLED: AtomicBool = AtomicBool::new(false);

struct VirtualCursorPlugin;

impl egui::Plugin for VirtualCursorPlugin {
    fn debug_name(&self) -> &'static str {
        "android_virtual_cursor"
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        paint_virtual_cursor(ui.ctx());
    }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        for vp in output.viewport_output.values() {
            for cmd in &vp.commands {
                if let egui::ViewportCommand::CursorVisible(visible) = cmd {
                    VIRTUAL_MOUSE.lock().unwrap_or_else(|e| e.into_inner()).cursor_hidden = !*visible;
                }
            }
        }
    }
}

fn paint_virtual_cursor(ctx: &egui::Context) {
    let (active, locked, pos) = {
        let vm = VIRTUAL_MOUSE.lock().unwrap_or_else(|e| e.into_inner());
        (vm.active && vm.is_captured && vm.initialized, vm.locked_look || vm.cursor_hidden, vm.pos)
    };
    if !active || locked {
        return;
    }
    if ctx.output(|o| o.cursor_icon == egui::CursorIcon::None) {
        return;
    }
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("android_virtual_cursor"));
    let painter = ctx.layer_painter(layer);
    let p0 = pos;
    let p1 = pos + egui::vec2(0.0, 17.0);
    let p2 = pos + egui::vec2(4.2, 13.0);
    let p3 = pos + egui::vec2(7.0, 19.0);
    let p4 = pos + egui::vec2(9.6, 17.8);
    let p5 = pos + egui::vec2(6.8, 12.0);
    let p6 = pos + egui::vec2(12.2, 12.0);
    let fill = Color32::WHITE;
    painter.add(Shape::convex_polygon(vec![p0, p1, p2], fill, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![p0, p2, p5], fill, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![p0, p5, p6], fill, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![p2, p3, p4, p5], fill, Stroke::NONE));
    painter.add(Shape::closed_line(
        vec![p0, p1, p2, p3, p4, p5, p6],
        Stroke::new(1.5, Color32::from_rgb(15, 15, 20)),
    ));
}

static QUEUE: Mutex<Vec<ImeOp>> = Mutex::new(Vec::new());

fn push_op(op: ImeOp) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(op);
    }
    super::lifecycle::wake();
}

/// Raw JNI bridge called from `crates/rustudio-android/src/lib.rs`.
///
/// # Safety
/// Must be called by the JVM with valid `JNIEnv*` and `jstring`.
pub unsafe fn jni_commit_text(env: *mut std::ffi::c_void, text: *mut std::ffi::c_void) {
    if env.is_null() || text.is_null() {
        return;
    }
    let env_ptr = env as *mut jni::sys::JNIEnv;
    let jstr = text as jni::sys::jstring;
    unsafe {
        let Some(vtable) = (*env_ptr).as_ref() else { return };
        let len = (vtable.v1_1.GetStringLength)(env_ptr, jstr);
        if len <= 0 {
            return;
        }
        let get_chars = vtable.v1_1.GetStringChars;
        let rel_chars = vtable.v1_1.ReleaseStringChars;
        let raw = get_chars(env_ptr, jstr, std::ptr::null_mut());
        if !raw.is_null() {
            let slice = std::slice::from_raw_parts(raw, len as usize);
            let s = String::from_utf16_lossy(slice);
            rel_chars(env_ptr, jstr, raw);
            on_commit_text(s);
        }
    }
}

/// Called from JNI `RustudioActivity.nativeImeCommitText`.
pub fn on_commit_text(text: String) {
    if !text.is_empty() {
        push_op(ImeOp::CommitText(text));
    }
}

/// Called from JNI `RustudioActivity.nativeImeDelete`.
pub fn on_delete(before: i32, after: i32) {
    if before > 0 || after > 0 {
        push_op(ImeOp::Delete { before, after });
    }
}

/// Called from JNI `RustudioActivity.nativeImeKey`.
pub fn on_key(code: i32, down: bool) {
    push_op(ImeOp::Key { code, down });
}

/// Called from JNI `RustudioActivity.nativeImeAction`.
pub fn on_action(action: i32) {
    push_op(ImeOp::Action(action));
}

/// Called from JNI `RustudioActivity.nativeHardwareKey` for hardware keyboard shortcuts.
pub fn on_hardware_key(code: i32, meta: i32, down: bool, repeat: bool) {
    push_op(ImeOp::HardwareKey { code, meta, down, repeat });
}

/// Updates virtual cursor visibility from `Game::cursor_cmd` (`ViewportCommand::CursorVisible`).
pub fn set_cursor_hidden(hidden: bool) {
    VIRTUAL_MOUSE.lock().unwrap_or_else(|e| e.into_inner()).cursor_hidden = hidden;
}

/// Called from JNI `RustudioActivity.nativeHardwareMouseDetected` when a hardware
/// mouse is detected (or when real finger touch switches back to touch mode).
/// Enqueued in `QUEUE` so mode transitions stay strictly ordered with queued `MouseCapture` ops.
pub fn on_hardware_mouse_detected(active: bool) {
    push_op(ImeOp::HardwareMouseDetected(active));
}

/// Called from JNI `RustudioActivity.nativePointerCaptureChanged` when Android grants
/// or revokes pointer capture (system gesture, Esc, notification shade, focus change).
pub fn on_pointer_capture_changed(has_capture: bool) {
    push_op(ImeOp::PointerCaptureChanged(has_capture));
}

/// Called from JNI `RustudioActivity.nativeMouseCapture` (API 26+ `OnCapturedPointerListener`).
pub fn on_mouse_capture(
    action: i32,
    dx: f32,
    dy: f32,
    button_state: i32,
    action_button: i32,
    scroll_x: f32,
    scroll_y: f32,
    abs_x: f32,
    abs_y: f32,
    is_captured: bool,
) {
    push_op(ImeOp::MouseCapture {
        action,
        dx,
        dy,
        button_state,
        action_button,
        scroll_x,
        scroll_y,
        abs_x,
        abs_y,
        is_captured,
    });
}

static IME_VISIBILITY_KNOWN: AtomicBool = AtomicBool::new(false);
static IME_VISIBLE: AtomicBool = AtomicBool::new(false);
static IME_BOTTOM_INSET_PX: AtomicI32 = AtomicI32::new(0);

/// Called from JNI `RustudioActivity.nativeImeVisibility` (API 30+ `OnApplyWindowInsetsListener`).
pub fn on_ime_visibility(visible: bool, bottom_inset_px: i32) {
    IME_VISIBLE.store(visible, Ordering::Relaxed);
    IME_BOTTOM_INSET_PX.store(bottom_inset_px.max(0), Ordering::Relaxed);
    IME_VISIBILITY_KNOWN.store(true, Ordering::Relaxed);
    super::lifecycle::wake();
}

/// Latest `(visible, bottom_inset_px)` pushed by `OnApplyWindowInsetsListener`
/// on API 30+, if at least one callback has arrived.
pub fn ime_visibility() -> Option<(bool, i32)> {
    IME_VISIBILITY_KNOWN.load(Ordering::Relaxed).then(|| {
        (
            IME_VISIBLE.load(Ordering::Relaxed),
            IME_BOTTOM_INSET_PX.load(Ordering::Relaxed),
        )
    })
}

/// Asks `RustudioActivity.setNativeImeOpen(open)` to attach or detach the soft
/// keyboard directly to `ImeProxyView`'s `InputConnection`. Returns `true` if
/// the call succeeded (false falls back to stock `AndroidApp::show_soft_input`).
pub fn set_ime_open(open: bool) -> bool {
    let res = super::jni::with_activity(|env, activity| {
        env.call_method(
            activity,
            jni_str!("setNativeImeOpen"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(open)],
        )?;
        Ok(())
    });
    match res {
        Ok(()) => true,
        Err(e) => {
            log::warn!("ime_bridge: setNativeImeOpen({open}) fallback: {e}");
            false
        }
    }
}

/// Resets `ImeProxyView.composingLen` and restarts input when focus switches
/// between text fields while the soft keyboard stays open.
pub fn reset_composing() {
    let _ = super::jni::with_activity(|env, activity| {
        env.call_method(
            activity,
            jni_str!("resetImeComposing"),
            jni_sig!("()V"),
            &[],
        )?;
        Ok(())
    });
}

fn push_key_tap(events: &mut Vec<Event>, key: Key) {
    events.push(Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    events.push(Event::Key {
        key,
        physical_key: Some(key),
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
}

fn map_android_modifiers(meta: i32) -> Modifiers {
    const META_SHIFT_ON: i32 = 0x0000_0001;
    const META_ALT_ON: i32 = 0x0000_0002;
    const META_CTRL_ON: i32 = 0x0000_1000;
    const META_META_ON: i32 = 0x0001_0000;
    let ctrl = (meta & META_CTRL_ON) != 0;
    let meta_key = (meta & META_META_ON) != 0;
    Modifiers {
        alt: (meta & META_ALT_ON) != 0,
        ctrl,
        shift: (meta & META_SHIFT_ON) != 0,
        mac_cmd: false,
        command: ctrl || meta_key,
    }
}

fn map_android_keycode(code: i32) -> Option<Key> {
    match code {
        7 => Some(Key::Num0),
        8 => Some(Key::Num1),
        9 => Some(Key::Num2),
        10 => Some(Key::Num3),
        11 => Some(Key::Num4),
        12 => Some(Key::Num5),
        13 => Some(Key::Num6),
        14 => Some(Key::Num7),
        15 => Some(Key::Num8),
        16 => Some(Key::Num9),
        29 => Some(Key::A),
        30 => Some(Key::B),
        31 => Some(Key::C),
        32 => Some(Key::D),
        33 => Some(Key::E),
        34 => Some(Key::F),
        35 => Some(Key::G),
        36 => Some(Key::H),
        37 => Some(Key::I),
        38 => Some(Key::J),
        39 => Some(Key::K),
        40 => Some(Key::L),
        41 => Some(Key::M),
        42 => Some(Key::N),
        43 => Some(Key::O),
        44 => Some(Key::P),
        45 => Some(Key::Q),
        46 => Some(Key::R),
        47 => Some(Key::S),
        48 => Some(Key::T),
        49 => Some(Key::U),
        50 => Some(Key::V),
        51 => Some(Key::W),
        52 => Some(Key::X),
        53 => Some(Key::Y),
        54 => Some(Key::Z),
        55 => Some(Key::Comma),
        56 => Some(Key::Period),
        61 => Some(Key::Tab),
        62 => Some(Key::Space),
        66 | 160 => Some(Key::Enter),
        67 => Some(Key::Backspace),
        68 => Some(Key::Backtick),
        69 => Some(Key::Minus),
        70 => Some(Key::Equals),
        71 => Some(Key::OpenBracket),
        72 => Some(Key::CloseBracket),
        73 => Some(Key::Backslash),
        74 => Some(Key::Semicolon),
        75 => Some(Key::Quote),
        76 => Some(Key::Slash),
        92 => Some(Key::PageUp),
        93 => Some(Key::PageDown),
        111 => Some(Key::Escape),
        112 => Some(Key::Delete),
        122 => Some(Key::Home),
        123 => Some(Key::End),
        124 => Some(Key::Insert),
        131 => Some(Key::F1),
        132 => Some(Key::F2),
        133 => Some(Key::F3),
        134 => Some(Key::F4),
        135 => Some(Key::F5),
        136 => Some(Key::F6),
        137 => Some(Key::F7),
        138 => Some(Key::F8),
        139 => Some(Key::F9),
        140 => Some(Key::F10),
        141 => Some(Key::F11),
        142 => Some(Key::F12),
        19 => Some(Key::ArrowUp),
        20 => Some(Key::ArrowDown),
        21 => Some(Key::ArrowLeft),
        22 => Some(Key::ArrowRight),
        _ => None,
    }
}

/// Drains queued native Android IME operations into `egui::RawInput::events`
/// inside `App::raw_input_hook`.
pub fn before_frame(ctx: &egui::Context, raw: &mut egui::RawInput) {
    if !CURSOR_PLUGIN_INSTALLED.swap(true, Ordering::Relaxed) {
        ctx.add_plugin(VirtualCursorPlugin);
    }
    let screen_rect = raw.screen_rect.unwrap_or_else(|| ctx.content_rect());
    if screen_rect.is_positive() {
        if let Ok(mut vm) = VIRTUAL_MOUSE.lock() {
            if !vm.initialized {
                vm.pos = screen_rect.center();
                vm.drag_pos = vm.pos;
                vm.initialized = true;
            }
        }
    }
    let ops = {
        let Ok(mut q) = QUEUE.lock() else { return };
        if q.is_empty() {
            return;
        }
        std::mem::take(&mut *q)
    };

    let mut current_modifiers = ctx.input(|i| i.modifiers);
    for op in ops {
        match op {
            ImeOp::CommitText(s) => {
                // Gboard's Enter/Search key sends commitText("\n") on multi-line
                // InputConnections: emit Key::Enter so single-line search boxes
                // (Toolbox, Explorer, Properties) submit AND Script Editor gets
                // normal Enter + Luau auto-indent!
                if s == "\n" || s == "\r\n" || s == "\r" {
                    push_key_tap(&mut raw.events, Key::Enter);
                } else if s.contains('\n') || s.contains('\r') || s.chars().count() > 8 {
                    let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
                    raw.events.push(Event::Paste(normalized));
                } else {
                    raw.events.push(Event::Text(s));
                }
            }
            ImeOp::Delete { before, after } => {
                for _ in 0..before.clamp(0, 64) {
                    push_key_tap(&mut raw.events, Key::Backspace);
                }
                for _ in 0..after.clamp(0, 64) {
                    push_key_tap(&mut raw.events, Key::Delete);
                }
            }
            ImeOp::Key { code, down } => {
                if let Some(key) = map_android_keycode(code) {
                    raw.events.push(Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: down,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    });
                }
            }
            ImeOp::HardwareKey { code, meta, down, repeat } => {
                let modifiers = map_android_modifiers(meta);
                if modifiers != current_modifiers {
                    raw.events.push(Event::ModifiersChanged(modifiers));
                    current_modifiers = modifiers;
                }
                if let Some(key) = map_android_keycode(code) {
                    if down && !repeat && modifiers.command && !modifiers.shift && !modifiers.alt {
                        match key {
                            Key::C => {
                                raw.events.push(Event::Copy);
                                continue;
                            }
                            Key::X => {
                                raw.events.push(Event::Cut);
                                continue;
                            }
                            Key::V => {
                                let text = super::clipboard::text_now()
                                    .or_else(super::clipboard::text)
                                    .unwrap_or_default();
                                if !text.is_empty() {
                                    raw.events.push(Event::Paste(text));
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                    raw.events.push(Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed: down,
                        repeat,
                        modifiers,
                    });
                }
            }
            ImeOp::Action(id) => match id {
                1 => {
                    // Select All (Ctrl+A)
                    raw.events.push(Event::Key {
                        key: Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::COMMAND,
                    });
                    raw.events.push(Event::Key {
                        key: Key::A,
                        physical_key: None,
                        pressed: false,
                        repeat: false,
                        modifiers: Modifiers::COMMAND,
                    });
                }
                2 => raw.events.push(Event::Cut),
                3 => raw.events.push(Event::Copy),
                4 => {
                    let text = super::clipboard::text_now()
                        .or_else(super::clipboard::text)
                        .unwrap_or_default();
                    if !text.is_empty() {
                        raw.events.push(Event::Paste(text));
                    }
                }
                _ => {}
            },
            ImeOp::HardwareMouseDetected(active) => {
                rustudio_engine::touch_input::set_captured_hardware_mouse(active);
                let mut vm = VIRTUAL_MOUSE.lock().unwrap_or_else(|e| e.into_inner());
                vm.active = active;
                if !active {
                    let prev_buttons = vm.buttons;
                    let release_pos = vm.drag_pos;
                    vm.is_captured = false;
                    vm.buttons = 0;
                    vm.locked_look = false;
                    vm.right_press_pos = None;
                    for (mask, button) in [
                        (1i32, PointerButton::Primary),
                        (2i32, PointerButton::Secondary),
                        (4i32, PointerButton::Middle),
                    ] {
                        if (prev_buttons & mask) != 0 {
                            raw.events.push(Event::PointerButton {
                                pos: release_pos,
                                button,
                                pressed: false,
                                modifiers: current_modifiers,
                            });
                        }
                    }
                }
            }
            ImeOp::PointerCaptureChanged(has_capture) => {
                let mut vm = VIRTUAL_MOUSE.lock().unwrap_or_else(|e| e.into_inner());
                vm.is_captured = has_capture;
                if !has_capture {
                    let prev_buttons = vm.buttons;
                    let release_pos = vm.drag_pos;
                    vm.buttons = 0;
                    vm.locked_look = false;
                    let anchor = vm.right_press_pos.take();
                    for (mask, button) in [
                        (1i32, PointerButton::Primary),
                        (2i32, PointerButton::Secondary),
                        (4i32, PointerButton::Middle),
                    ] {
                        if (prev_buttons & mask) != 0 {
                            raw.events.push(Event::PointerButton {
                                pos: release_pos,
                                button,
                                pressed: false,
                                modifiers: current_modifiers,
                            });
                        }
                    }
                    if let Some(a) = anchor {
                        vm.pos = a;
                        vm.drag_pos = a;
                        raw.events.push(Event::PointerMoved(a));
                    }
                }
            }
            ImeOp::MouseCapture {
                action,
                dx,
                dy,
                button_state,
                action_button,
                scroll_x,
                scroll_y,
                abs_x,
                abs_y,
                is_captured,
            } => {
                const ACTION_DOWN: i32 = 0;
                const ACTION_UP: i32 = 1;
                const ACTION_CANCEL: i32 = 3;
                const ACTION_SCROLL: i32 = 8;
                const ACTION_HOVER_EXIT: i32 = 10;
                const ACTION_BUTTON_PRESS: i32 = 11;
                const ACTION_BUTTON_RELEASE: i32 = 12;
                const BUTTON_PRIMARY: i32 = 1;
                const BUTTON_SECONDARY: i32 = 2;
                const BUTTON_TERTIARY: i32 = 4;

                if !rustudio_engine::touch_input::captured_hardware_mouse() {
                    rustudio_engine::touch_input::set_captured_hardware_mouse(true);
                }

                let ppp = ctx.pixels_per_point().max(0.5);
                let delta = egui::vec2(dx / ppp, dy / ppp);
                let mut vm = VIRTUAL_MOUSE.lock().unwrap_or_else(|e| e.into_inner());
                vm.active = true;
                vm.is_captured = is_captured;
                if !vm.initialized && screen_rect.is_positive() {
                    vm.pos = if !is_captured && abs_x >= 0.0 && abs_y >= 0.0 {
                        Pos2::new(abs_x / ppp, abs_y / ppp)
                    } else {
                        screen_rect.center()
                    };
                    vm.drag_pos = vm.pos;
                    vm.initialized = true;
                    raw.events.push(Event::PointerMoved(vm.pos));
                }

                if !is_captured && action == ACTION_HOVER_EXIT {
                    raw.events.push(Event::PointerGone);
                    continue;
                }

                let mut new_buttons = button_state;
                if action == ACTION_BUTTON_PRESS && action_button != 0 {
                    new_buttons |= action_button;
                } else if action == ACTION_BUTTON_RELEASE && action_button != 0 {
                    new_buttons &= !action_button;
                } else if !is_captured && action == ACTION_DOWN && button_state == 0 && action_button == 0 {
                    new_buttons |= BUTTON_PRIMARY;
                } else if !is_captured && action == ACTION_UP && button_state == 0 && action_button == 0 {
                    new_buttons &= !BUTTON_PRIMARY;
                } else if action == ACTION_CANCEL {
                    new_buttons = 0;
                }

                let right_held = (new_buttons & BUTTON_SECONDARY) != 0
                    && (vm.buttons & BUTTON_SECONDARY) != 0;

                if !is_captured && abs_x >= 0.0 && abs_y >= 0.0 {
                    let mut target = Pos2::new(abs_x / ppp, abs_y / ppp);
                    if screen_rect.is_positive() {
                        target.x = target.x.clamp(screen_rect.min.x, screen_rect.max.x);
                        target.y = target.y.clamp(screen_rect.min.y, screen_rect.max.y);
                    }
                    if delta.x != 0.0 || delta.y != 0.0 {
                        raw.events.push(Event::MouseMoved(delta));
                    }
                    if right_held {
                        vm.drag_pos += delta;
                        raw.events.push(Event::PointerMoved(vm.drag_pos));
                    } else if target != vm.pos || delta.x != 0.0 || delta.y != 0.0 {
                        vm.pos = target;
                        vm.drag_pos = vm.pos;
                        raw.events.push(Event::PointerMoved(vm.pos));
                    }
                } else if delta.x != 0.0 || delta.y != 0.0 {
                    raw.events.push(Event::MouseMoved(delta));
                    if !right_held && !vm.cursor_hidden {
                        vm.pos += delta;
                        if screen_rect.is_positive() {
                            vm.pos.x = vm.pos.x.clamp(screen_rect.min.x, screen_rect.max.x);
                            vm.pos.y = vm.pos.y.clamp(screen_rect.min.y, screen_rect.max.y);
                        }
                        vm.drag_pos = vm.pos;
                        raw.events.push(Event::PointerMoved(vm.pos));
                    } else if right_held {
                        // Keep virtual cursor anchored at right-press position while
                        // advancing egui's pointer delta unclamped for infinite camera orbit.
                        vm.drag_pos += delta;
                        raw.events.push(Event::PointerMoved(vm.drag_pos));
                    }
                }

                let prev_buttons = vm.buttons;
                if new_buttons != prev_buttons {
                    for (mask, button) in [
                        (BUTTON_PRIMARY, PointerButton::Primary),
                        (BUTTON_SECONDARY, PointerButton::Secondary),
                        (BUTTON_TERTIARY, PointerButton::Middle),
                    ] {
                        let was = (prev_buttons & mask) != 0;
                        let now = (new_buttons & mask) != 0;
                        if was != now {
                            if mask == BUTTON_SECONDARY && !now {
                                // Emit release at drag_pos FIRST so egui does not misclassify
                                // a camera orbit drag as a 0px right-click context menu, then restore anchor.
                                let release_pos = vm.drag_pos;
                                raw.events.push(Event::PointerButton {
                                    pos: release_pos,
                                    button,
                                    pressed: false,
                                    modifiers: current_modifiers,
                                });
                                if let Some(anchor) = vm.right_press_pos.take() {
                                    vm.pos = anchor;
                                }
                                vm.drag_pos = vm.pos;
                                vm.locked_look = false;
                                raw.events.push(Event::PointerMoved(vm.pos));
                                continue;
                            }
                            if mask == BUTTON_SECONDARY && now {
                                vm.right_press_pos = Some(vm.pos);
                                vm.drag_pos = vm.pos;
                                vm.locked_look = true;
                            }
                            let event_pos = vm.pos;
                            raw.events.push(Event::PointerMoved(event_pos));
                            raw.events.push(Event::PointerButton {
                                pos: event_pos,
                                button,
                                pressed: now,
                                modifiers: current_modifiers,
                            });
                        }
                    }
                    vm.buttons = new_buttons;
                }

                if (action == ACTION_SCROLL || scroll_x != 0.0 || scroll_y != 0.0)
                    && (scroll_x != 0.0 || scroll_y != 0.0)
                {
                    raw.events.push(Event::MouseWheel {
                        unit: MouseWheelUnit::Line,
                        delta: egui::vec2(scroll_x, scroll_y),
                        phase: TouchPhase::Move,
                        modifiers: current_modifiers,
                    });
                }
            }
        }
    }
    ctx.request_repaint();
}
