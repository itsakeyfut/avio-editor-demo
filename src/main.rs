mod state;
mod thumbnail;
use state::{AppState, ImportedClip};

fn main() -> eframe::Result<()> {
    env_logger::init();
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
        Box::new(|_cc| Ok(Box::new(AvioEditorApp::default()))),
    )
}

#[derive(Default)]
struct AvioEditorApp {
    state: AppState,
}

impl eframe::App for AvioEditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain completed thumbnail results each frame.
        while let Ok((path, w, h, rgb)) = self.state.thumbnail_rx.try_recv() {
            let image = egui::ColorImage::from_rgb([w as usize, h as usize], &rgb);
            let texture =
                ctx.load_texture(path.to_string_lossy(), image, egui::TextureOptions::LINEAR);
            if let Some(clip) = self.state.clips.iter_mut().find(|c| c.path == path) {
                clip.thumbnail = Some(texture);
            }
        }

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
                            Ok(info) => {
                                let has_video = info.primary_video().is_some();
                                self.state.clips.push(ImportedClip {
                                    path: path.clone(),
                                    info,
                                    thumbnail: None,
                                    proxy_path: None,
                                });
                                if has_video {
                                    let tx = self.state.thumbnail_tx.clone();
                                    let path_for_task = path.clone();
                                    tokio::task::spawn_blocking(move || {
                                        if let Some((w, h, rgb)) =
                                            thumbnail::select_best_thumbnail(&path_for_task)
                                        {
                                            let _ = tx.send((path_for_task, w, h, rgb));
                                        }
                                    });
                                }
                            }
                            Err(e) => log::warn!("probe failed for {path:?}: {e}"),
                        }
                    }
                }

                ui.separator();

                let mut clicked_idx: Option<usize> = None;
                for (idx, clip) in self.state.clips.iter().enumerate() {
                    let selected = self.state.selected_clip_index == Some(idx);
                    ui.horizontal(|ui| {
                        match &clip.thumbnail {
                            Some(tex) => {
                                ui.image(egui::load::SizedTexture::new(
                                    tex.id(),
                                    egui::vec2(48.0, 27.0),
                                ));
                            }
                            None => {
                                ui.label("\u{1F3AC}");
                            }
                        }
                        let name = clip.path.file_name().unwrap_or_default().to_string_lossy();
                        if ui.selectable_label(selected, name.as_ref()).clicked() {
                            clicked_idx = Some(idx);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(clip.duration_label());
                        });
                    });
                }
                if let Some(idx) = clicked_idx {
                    self.state.selected_clip_index = Some(idx);
                }

                if let Some(idx) = self.state.selected_clip_index
                    && let Some(clip) = self.state.clips.get(idx)
                {
                    ui.separator();
                    egui::Grid::new("meta_grid")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            let info = &clip.info;

                            ui.label("Container");
                            ui.label(info.format());
                            ui.end_row();

                            if let Some(v) = info.primary_video() {
                                ui.label("Video");
                                ui.label(v.codec().display_name());
                                ui.end_row();

                                ui.label("Size");
                                ui.label(format!("{}×{}", v.width(), v.height()));
                                ui.end_row();

                                ui.label("FPS");
                                ui.label(format!("{:.3}", v.fps()));
                                ui.end_row();

                                if let Some(br) = v.bitrate() {
                                    ui.label("V-bitrate");
                                    ui.label(format!("{} kb/s", br / 1000));
                                    ui.end_row();
                                }

                                ui.label("Color");
                                ui.label(v.color_space().name());
                                ui.end_row();
                            }

                            if let Some(a) = info.primary_audio() {
                                ui.label("Audio");
                                ui.label(a.codec().display_name());
                                ui.end_row();

                                ui.label("Rate");
                                ui.label(format!("{} Hz", a.sample_rate()));
                                ui.end_row();

                                ui.label("Ch");
                                ui.label(format!("{}", a.channels()));
                                ui.end_row();

                                if let Some(br) = a.bitrate() {
                                    ui.label("A-bitrate");
                                    ui.label(format!("{} kb/s", br / 1000));
                                    ui.end_row();
                                }
                            }

                            ui.label("Duration");
                            ui.label(clip.duration_label());
                            ui.end_row();
                        });
                }
            });

        // 4. Center: Source Monitor (must be last)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Source Monitor");
        });
    }
}
