mod state;
use state::{AppState, ImportedClip};

fn main() -> eframe::Result<()> {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("avio-editor-demo")
            .with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "avio-editor-demo",
        options,
        Box::new(|_cc| Ok(Box::new(AvioEditorApp::default()))),
    )
}

#[derive(Default)]
struct AvioEditorApp {
    state: AppState,
}

impl eframe::App for AvioEditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Top menu bar (must come before all other panels)
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |_ui| {});
                ui.menu_button("Export", |_ui| {});
            });
        });

        // 2. Bottom: Timeline (must come before SidePanel and CentralPanel)
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(200.0)
            .show(ctx, |ui| {
                ui.heading("Timeline");
            });

        // 3. Left: Clip Browser
        egui::SidePanel::left("clip_browser")
            .resizable(true)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.heading("Clip Browser");
                ui.separator();

                if ui.button("Import").clicked()
                    && let Some(paths) = rfd::FileDialog::new()
                        .add_filter(
                            "Video / Audio",
                            &["mp4", "mov", "mkv", "avi", "mp3", "aac", "wav", "flac"],
                        )
                        .pick_files()
                {
                    for path in paths {
                        match avio::open(&path) {
                            Ok(info) => self.state.clips.push(ImportedClip {
                                path,
                                info,
                                thumbnail: None,
                                proxy_path: None,
                            }),
                            Err(e) => log::warn!("probe failed for {path:?}: {e}"),
                        }
                    }
                }

                ui.separator();

                for clip in &self.state.clips {
                    ui.horizontal(|ui| {
                        ui.label("\u{1F3AC}");
                        ui.label(
                            clip.path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .as_ref(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(clip.duration_label());
                        });
                    });
                }
            });

        // 4. Center: Source Monitor (must be last)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Source Monitor");
        });
    }
}
