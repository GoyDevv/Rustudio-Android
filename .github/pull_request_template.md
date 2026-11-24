## Description

<!-- Describe what problem this PR solves or what feature it adds. -->

## Motivation & Parity Basis

<!--
Per AGENTS.md and CONTRIBUTING.md, state what this change is based on:
- Roblox Studio's behavior
- The Roblox API dump (api-dump.json)
- Concrete measurement / benchmark
- An estimate (documented per docs/MECHANICS.md)
-->

## Platforms & Devices Tested

<!-- List platforms and devices tested (e.g. Linux x86_64 Vulkan, Windows 11, macOS, Android arm64 / Vivo Y27 Mali-G52 MC2). -->
- [ ] Linux x86_64
- [ ] Windows x64
- [ ] macOS
- [ ] Android arm64-v8a (Device: `...`, GPU: `...`)

## Visual Verification

<!--
Per AGENTS.md, look at screenshots. A test that passes while the picture is wrong isn't a fix.
Attach screenshots, FPS comparisons, or screen recordings if UI or rendering is changed.
-->

## Verification Checklist

Please verify the following before requesting review:

### Code Quality & Standards
- [ ] Ran `cargo fmt --check` and code follows Rust formatting standards.
- [ ] No new compiler warnings or `clippy` warnings introduced.
- [ ] Kept changes focused and minimal without unrelated refactors.
- [ ] Platform-specific optimizations cleanly gated (`#[cfg(target_os = "android")]` / `cfg!(target_os = "android")`).

### Tests & Verification
- [ ] Ran core test suites (`cargo test -p rustudio-core --lib`, `cargo test -p rustudio-render --lib`, `cargo test -p rustudio-net --lib`).
- [ ] Ran GPU pipeline build test if modifying shaders (`cargo test -p rustudio-render --lib -- render::gpu_tests::renderer_pipelines_build_on_this_gpu`).
- [ ] Added or updated unit tests for bug fixes, settings, or protocol changes.

### Android & Mobile Engineering Rules (if applicable)
- [ ] Native Android graphics drivers only (standard Vulkan 1.1+ / OpenGL ES 3.0+; no Turnip/PanVK/adrenotools).
- [ ] Never silently downgrades user quality settings.
- [ ] Touch & multi-touch isolation respected (floating panels/joystick do not leak pointer events to 3D camera).
- [ ] `crates/rustudio-android/manifest.yaml` maintains package ID `com.logosforgestudios.rustudio`.
- [ ] Built APK contains only `lib/arm64-v8a/librustudio_android.so`.

### Documentation & Engine Gaps
- [ ] New tuning values documented in `crates/rustudio-core/src/mech.rs`, `mechanics.json`, and `docs/MECHANICS.md`.
- [ ] Engine API checked against `api-dump.json` and updated `docs/ENGINE-GAPS.md` if an API gap is closed.
- [ ] Relevant documentation updated (`README.md`, `docs/ANDROID.md`, `docs/DEVELOPING.md`).
