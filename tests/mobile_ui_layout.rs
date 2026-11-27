//! Pure-CPU headless `egui::Context` UI layout regression tests for compact
//! mobile landscape viewports (`800×360` and `900×410`).

use egui::{Align, Align2, Id, Layout, Pos2, RawInput, Rect, ScrollArea, Vec2, pos2, vec2};
use egui_dock::{DockArea, DockState, NodeIndex, SurfaceIndex, TabIndex, TabPath, TabViewer};
use rustudio::asset_picker::AssetPicker;
use rustudio::mobile_windows::default_placement_for;

const MOBILE_VIEWPORTS: [Vec2; 2] = [vec2(800.0, 360.0), vec2(900.0, 410.0)];

fn run_frames(ctx: &egui::Context, viewport: Vec2, frames: usize, mut ui_fn: impl FnMut(&mut egui::Ui)) {
    for _ in 0..frames {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, viewport)),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| ui_fn(ui));
        output.textures_delta.clear();
    }
}

struct DummyViewer;

impl TabViewer for DummyViewer {
    type Tab = &'static str;

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        (*tab).into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        ui.label(*tab);
    }
}

#[test]
fn egui_dock_floating_min_size_and_viewport_clamping() {
    for viewport in MOBILE_VIEWPORTS {
        let screen = Rect::from_min_size(Pos2::ZERO, viewport);
        let ctx = egui::Context::default();
        let mut dock = DockState::new(vec!["Viewport", "Toolbox"]);
        let mut viewer = DummyViewer;

        // Detach a tab from the main surface with a tiny rect; should enforce >= 340×320.
        let main_detached = dock.detach_tab(
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(1)),
            Rect::from_min_size(pos2(viewport.x - 40.0, viewport.y - 40.0), vec2(120.0, 100.0)),
        );
        let main_rect = dock
            .get_window_state(main_detached)
            .and_then(|ws| ws.rect_or_next())
            .expect("main detached window rect");
        assert!(
            main_rect.width() >= 340.0 && main_rect.height() >= 320.0,
            "viewport {viewport:?}: main detached size {:?} below 340×320",
            main_rect.size(),
        );

        // Detach a tab from a non-main floating surface with a tiny rect; should enforce >= 260×220.
        let float_src = dock.add_window(vec!["Explorer", "Properties"]);
        let non_main_detached = dock.detach_tab(
            TabPath::new(float_src, NodeIndex::root(), TabIndex(1)),
            Rect::from_min_size(pos2(viewport.x - 20.0, viewport.y - 20.0), vec2(100.0, 90.0)),
        );
        let non_main_rect = dock
            .get_window_state(non_main_detached)
            .and_then(|ws| ws.rect_or_next())
            .expect("non-main detached window rect");
        assert!(
            non_main_rect.width() >= 260.0 && non_main_rect.height() >= 220.0,
            "viewport {viewport:?}: non-main detached size {:?} below 260×220",
            non_main_rect.size(),
        );

        // Render two frames so egui_dock clamps floating windows to `screen`.
        run_frames(&ctx, viewport, 2, |ui| {
            DockArea::new(&mut dock)
                .window_bounds(screen)
                .show(ui.ctx(), &mut viewer);
        });

        for surf in [main_detached, non_main_detached] {
            let rendered = dock
                .get_window_state(surf)
                .map(|ws| ws.rect())
                .expect("rendered window state");
            assert!(
                screen.expand(1.0).contains_rect(rendered),
                "viewport {viewport:?}: floating surface {surf:?} rect {rendered:?} escaped {screen:?}",
            );
        }
    }
}

#[test]
fn mobile_windows_default_toolbox_size() {
    for viewport in MOBILE_VIEWPORTS {
        let screen = Rect::from_min_size(Pos2::ZERO, viewport);
        let (pos, size) = default_placement_for("toolbox", screen, &[]);
        assert!(
            size.x >= 320.0 && size.y >= 260.0,
            "viewport {viewport:?}: default Toolbox size {size:?} below 320×260",
        );
        let rect = Rect::from_min_size(pos, size);
        assert!(
            screen.contains_rect(rect),
            "viewport {viewport:?}: default Toolbox rect {rect:?} escaped {screen:?}",
        );
    }
}

#[test]
fn horizontal_right_to_left_row_inside_scroll_area_stays_compact() {
    for viewport in MOBILE_VIEWPORTS {
        let screen = Rect::from_min_size(Pos2::ZERO, viewport);
        let ctx = egui::Context::default();
        let mut row_rect = Rect::NOTHING;

        run_frames(&ctx, viewport, 2, |ui| {
            let win_size = vec2(
                (screen.width() - 24.0).clamp(320.0, 520.0),
                (screen.height() - 24.0).clamp(260.0, 440.0),
            );
            egui::Window::new("Collaborate Row Regression")
                .id(Id::new("mobile_ui_layout_rtl_row_window"))
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(win_size)
                .collapsible(false)
                .resizable(false)
                .constrain_to(screen.shrink(8.0))
                .show(ui.ctx(), |ui| {
                    ScrollArea::vertical()
                        .id_salt("mobile_ui_layout_rtl_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            row_rect = ui
                                .horizontal(|ui| {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let _ = ui.button("Remove");
                                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                            ui.label("Teammate (@teammate)");
                                        });
                                    });
                                })
                                .response
                                .rect;
                        });
                });
        });

        assert!(
            row_rect.height() > 0.0 && row_rect.height() <= 40.0,
            "viewport {viewport:?}: right-to-left row inside ScrollArea expanded to {row_rect:?}",
        );
    }
}

#[test]
fn mobile_modals_stay_contained_within_shrunk_screen() {
    for viewport in MOBILE_VIEWPORTS {
        let screen = Rect::from_min_size(Pos2::ZERO, viewport);
        let bounds = screen.shrink(8.0);

        // 1. AssetPicker (PasteId tab)
        {
            let ctx = egui::Context::default();
            let mut picker = AssetPicker::new();
            picker.open_for(
                vec![rbx_types::Ref::new()],
                "ImageLabel".to_string(),
                "Image".to_string(),
                false,
                String::new(),
            );
            run_frames(&ctx, viewport, 2, |ui| {
                let _ = picker.show(ui.ctx(), None);
            });
            let rect = ctx
                .memory(|m| m.area_rect(Id::new("rustudio_asset_picker_window")))
                .expect("AssetPicker window rect");
            assert!(
                bounds.contains_rect(rect),
                "viewport {viewport:?}: AssetPicker rect {rect:?} escaped {bounds:?}",
            );
        }

        // 2. Collaborate modal sizing contract (`src/cloud_ui.rs`)
        {
            let ctx = egui::Context::default();
            let collab_id = Id::new("live_collaborators_window");
            run_frames(&ctx, viewport, 2, |ui| {
                let win_size = vec2(
                    (screen.width() - 24.0).clamp(320.0, 520.0),
                    (screen.height() - 24.0).clamp(260.0, 440.0),
                );
                egui::Window::new("Collaborate — Team Create")
                    .id(collab_id)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .fixed_size(win_size)
                    .min_size(win_size)
                    .max_size(win_size)
                    .collapsible(false)
                    .resizable(false)
                    .constrain_to(bounds)
                    .show(ui.ctx(), |ui| {
                        ScrollArea::vertical()
                            .id_salt("live_collaborators_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.label("Collaborators");
                            });
                    });
            });
            let rect = ctx
                .memory(|m| m.area_rect(collab_id))
                .expect("Collaborate modal rect");
            assert!(
                bounds.contains_rect(rect),
                "viewport {viewport:?}: Collaborate modal rect {rect:?} escaped {bounds:?}",
            );
        }

        // 3. File Browser modal sizing contract (`src/android/storage.rs`)
        {
            let ctx = egui::Context::default();
            let picker_id = Id::new("android-file-picker");
            run_frames(&ctx, viewport, 2, |ui| {
                let win_size = vec2(
                    (screen.width() - 24.0).clamp(320.0, 680.0),
                    (screen.height() - 24.0).clamp(260.0, 440.0),
                );
                egui::Window::new("Open File")
                    .id(picker_id)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .fixed_size(win_size)
                    .min_size(win_size)
                    .max_size(win_size)
                    .collapsible(false)
                    .resizable(false)
                    .constrain_to(bounds)
                    .show(ui.ctx(), |ui| {
                        ScrollArea::vertical()
                            .id_salt("android_file_picker_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.label("Places/");
                            });
                    });
            });
            let rect = ctx
                .memory(|m| m.area_rect(picker_id))
                .expect("File Browser modal rect");
            assert!(
                bounds.contains_rect(rect),
                "viewport {viewport:?}: File Browser modal rect {rect:?} escaped {bounds:?}",
            );
        }
    }
}
