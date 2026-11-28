//! Native Android `InputConnection` bridge from `RustudioActivity.java` directly
//! into RusTudio's real egui text fields (Script Editor, Properties, etc.) with
//! zero fake EditText overlay.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

use egui::{Event, Key, Modifiers};
use jni::objects::JValue;
use jni::{jni_sig, jni_str};

enum ImeOp {
    CommitText(String),
    Delete { before: i32, after: i32 },
    Key { code: i32, down: bool },
    HardwareKey { code: i32, meta: i32, down: bool, repeat: bool },
    Action(i32),
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
    let ops = {
        let Ok(mut q) = QUEUE.lock() else { return };
        if q.is_empty() {
            return;
        }
        std::mem::take(&mut *q)
    };

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
        }
    }
    ctx.request_repaint();
}
