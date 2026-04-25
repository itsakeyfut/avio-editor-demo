mod analysis;
mod export;
mod gif;
mod player;
mod presets;
mod proxy;
mod sprite;
mod state;
mod thumbnail;
mod trim;
mod ui;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let rt = tokio::runtime::Runtime::new().map_err(|e| eframe::Error::AppCreation(Box::new(e)))?;
    let _rt_guard = rt.enter();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("avio-editor-demo")
            .with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "avio-editor-demo",
        options,
        Box::new(|cc| {
            ui::theme::apply(&cc.egui_ctx);
            Ok(Box::new(AvioEditorApp::default()))
        }),
    )
}

#[derive(Default)]
struct AvioEditorApp {
    state: state::AppState,
}

impl eframe::App for AvioEditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ui::drain_background_jobs(&mut self.state, ctx);

        // Apply the user-selected theme every frame.
        ctx.set_theme(self.state.theme_preference);

        // 1. Top menu bar (must come before all other panels)
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |_ui| {});
                ui.menu_button("Edit", |ui| {
                    let can_undo = !self.state.undo_stack.is_empty();
                    let can_redo = !self.state.redo_stack.is_empty();
                    let undo_label = self
                        .state
                        .undo_stack
                        .last()
                        .map(|c| format!("Undo {}", c.label()))
                        .unwrap_or_else(|| "Undo".to_string());
                    let redo_label = self
                        .state
                        .redo_stack
                        .last()
                        .map(|c| format!("Redo {}", c.label()))
                        .unwrap_or_else(|| "Redo".to_string());
                    if ui
                        .add_enabled(
                            can_undo,
                            egui::Button::new(undo_label).shortcut_text("Ctrl+Z"),
                        )
                        .clicked()
                    {
                        self.state.apply_undo();
                        ui.close();
                    }
                    if ui
                        .add_enabled(
                            can_redo,
                            egui::Button::new(redo_label).shortcut_text("Ctrl+Y"),
                        )
                        .clicked()
                    {
                        self.state.apply_redo();
                        ui.close();
                    }
                });
                ui.menu_button("Export", |_ui| {});
                ui.menu_button("View", |ui| {
                    ui.label("Theme");
                    ui.separator();
                    for (pref, label) in [
                        (egui::ThemePreference::System, "System"),
                        (egui::ThemePreference::Dark, "Dark"),
                        (egui::ThemePreference::Light, "Light"),
                    ] {
                        if ui
                            .radio_value(&mut self.state.theme_preference, pref, label)
                            .clicked()
                        {
                            ui.close();
                        }
                    }
                });
            });
        });

        // 2. Bottom: Timeline (must come before SidePanel and CentralPanel)
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(200.0)
            .show(ctx, |ui| {
                ui::timeline::show(&mut self.state, ui);
            });

        // 3. Left: Clip Browser
        egui::SidePanel::left("clip_browser")
            .resizable(true)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui::clip_browser::show(&mut self.state, ui, ctx);
            });

        // 4. Center: Source Monitor (must be last)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui::monitor::show(&mut self.state, ui, ctx);
        });
    }
}
