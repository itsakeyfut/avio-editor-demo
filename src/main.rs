mod analysis;
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
        while let Ok((idx, scenes)) = self.state.scene_rx.try_recv() {
            if let Some(clip) = self.state.clips.get_mut(idx) {
                clip.scenes = scenes;
            }
        }
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
                ui.separator();

                let pps = self.state.timeline.pixels_per_second;
                let available_width = ui.available_width();
                let (_, ruler_rect) = ui.allocate_space(egui::vec2(available_width, 24.0));
                let painter = ui.painter_at(ruler_rect);

                painter.rect_filled(ruler_rect, 0.0, egui::Color32::from_gray(40));

                // Time tick marks every 5 s
                let mut t = 0.0f32;
                while t * pps < ruler_rect.width() {
                    let x = ruler_rect.left() + t * pps;
                    painter.vline(
                        x,
                        ruler_rect.y_range(),
                        egui::Stroke::new(1.0, egui::Color32::GRAY),
                    );
                    painter.text(
                        egui::pos2(x + 2.0, ruler_rect.top() + 2.0),
                        egui::Align2::LEFT_TOP,
                        format!("{t:.0}s"),
                        egui::FontId::monospace(10.0),
                        egui::Color32::GRAY,
                    );
                    t += 5.0;
                }

                // Orange scene-change markers for V1 clips
                for tc in &self.state.timeline.tracks[0].clips {
                    if let Some(source) = self.state.clips.get(tc.source_index) {
                        for &scene_ts in &source.scenes {
                            let track_ts = tc.start_on_track + scene_ts;
                            let x = ruler_rect.left() + track_ts.as_secs_f32() * pps;
                            if x >= ruler_rect.left() && x <= ruler_rect.right() {
                                painter.vline(
                                    x,
                                    ruler_rect.y_range(),
                                    egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 165, 0)),
                                );
                            }
                        }
                    }
                }
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
                                    scenes: Vec::new(),
                                });
                                let clip_idx = self.state.clips.len() - 1;
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
                                    let scene_tx = self.state.scene_tx.clone();
                                    let path_for_scene = path.clone();
                                    tokio::task::spawn_blocking(move || {
                                        let scenes = analysis::detect_scenes(&path_for_scene);
                                        let _ = scene_tx.send((clip_idx, scenes));
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
                    ui.separator();
                    if ui.button("Add to V1").clicked() {
                        let start = self.state.timeline.tracks[0]
                            .clips
                            .last()
                            .map(|tc| {
                                tc.start_on_track
                                    + self.state.clips[tc.source_index].info.duration()
                            })
                            .unwrap_or_default();
                        self.state.timeline.tracks[0]
                            .clips
                            .push(state::TimelineClip {
                                source_index: idx,
                                start_on_track: start,
                            });
                    }
                }
            });

        // 4. Center: Source Monitor (must be last)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Source Monitor");
        });
    }
}
