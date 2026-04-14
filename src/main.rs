mod analysis;
mod gif;
mod player;
mod state;
mod thumbnail;
mod trim;
use std::sync::Arc;
use std::time::Duration;

use state::{AppState, GifStatus, ImportedClip, TrimStatus};

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
        // Drain completed trim jobs each frame.
        // Collect outcomes first to avoid borrowing trim_jobs and clips simultaneously.
        let mut trim_done: Vec<std::path::PathBuf> = Vec::new();
        self.state
            .trim_jobs
            .retain(|job| match job.status.lock().unwrap().clone() {
                TrimStatus::Running => true,
                TrimStatus::Done(path) => {
                    trim_done.push(path);
                    false
                }
                TrimStatus::Failed(msg) => {
                    log::warn!("trim failed: {msg}");
                    false
                }
            });
        for path in trim_done {
            match avio::open(&path) {
                Ok(info) => {
                    let has_video = info.primary_video().is_some();
                    let clip_idx = self.state.clips.len();
                    self.state.clips.push(ImportedClip {
                        path: path.clone(),
                        info,
                        thumbnail: None,
                        proxy_path: None,
                        scenes: Vec::new(),
                        in_point: None,
                        out_point: None,
                    });
                    if has_video {
                        let tx = self.state.thumbnail_tx.clone();
                        let p = path.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Some((w, h, rgb)) = thumbnail::select_best_thumbnail(&p) {
                                let _ = tx.send((p, w, h, rgb));
                            }
                        });
                        let scene_tx = self.state.scene_tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let scenes = analysis::detect_scenes(&path);
                            let _ = scene_tx.send((clip_idx, scenes));
                        });
                    }
                }
                Err(e) => log::warn!("probe failed for trimmed clip {path:?}: {e}"),
            }
        }

        // Drain completed GIF jobs each frame.
        let mut gif_done: Vec<std::path::PathBuf> = Vec::new();
        self.state
            .gif_jobs
            .retain(|job| match job.status.lock().unwrap().clone() {
                GifStatus::Running => true,
                GifStatus::Done(path) => {
                    gif_done.push(path);
                    false
                }
                GifStatus::Failed(msg) => {
                    log::warn!("GIF export failed: {msg}");
                    false
                }
            });
        for path in gif_done {
            log::info!("GIF exported: {}", path.display());
        }

        // Receive stop handle from a freshly spawned player thread.
        if let Some(rx) = &self.state.pending_stop_rx
            && let Ok(stop) = rx.try_recv()
        {
            self.state.player_stop = Some(stop);
            self.state.pending_stop_rx = None;
        }

        // Poll for the latest decoded frame from the player sink.
        if let Ok(mut guard) = self.state.frame_handle.try_lock()
            && let Some(frame) = guard.take()
        {
            // avio API gap: PreviewPlayer has no current_pts() — track pts
            // from TimedRgbaSink::push_frame() into AppState::current_pts.
            self.state.current_pts = Some(frame.pts);
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.data,
            );
            match &mut self.state.preview_texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => {
                    self.state.preview_texture = Some(ctx.load_texture(
                        "source_monitor",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            ctx.request_repaint();
        }

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
                                    in_point: None,
                                    out_point: None,
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
                let mut dbl_clicked_idx: Option<usize> = None;
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
                        let resp = ui.selectable_label(selected, name.as_ref());
                        if resp.clicked() {
                            clicked_idx = Some(idx);
                        }
                        if resp.double_clicked() {
                            dbl_clicked_idx = Some(idx);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(clip.duration_label());
                        });
                    });
                }
                if let Some(idx) = clicked_idx {
                    self.state.selected_clip_index = Some(idx);
                }
                if let Some(idx) = dbl_clicked_idx {
                    self.state.selected_clip_index = Some(idx);
                    // Stop any current player.
                    if let Some(stop) = self.state.player_stop.take() {
                        stop.store(true, std::sync::atomic::Ordering::Release);
                    }
                    self.state.player_thread = None;
                    self.state.pending_stop_rx = None;
                    self.state.monitor_clip_index = Some(idx);

                    // Only launch a player if the clip has a video stream.
                    // PreviewPlayer::open() fails for audio-only files because
                    // DecodeBuffer requires a video stream — avio API gap: a
                    // dedicated AudioPlayer (or an audio-only path in
                    // PreviewPlayer) would be needed.
                    let has_video = self
                        .state
                        .clips
                        .get(idx)
                        .map(|c| c.info.primary_video().is_some())
                        .unwrap_or(false);
                    if has_video
                        && let Some(path) = self.state.clips.get(idx).map(|c| c.path.clone())
                    {
                        let (thread, stop_rx) = player::spawn_player(
                            path,
                            Arc::clone(&self.state.frame_handle),
                            ctx.clone(),
                            None,
                        );
                        self.state.player_thread = Some(thread);
                        self.state.pending_stop_rx = Some(stop_rx);
                    }
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
                    let can_trim = clip.in_point.is_some() && clip.out_point.is_some();
                    if ui
                        .add_enabled(can_trim, egui::Button::new("Trim & Save"))
                        .clicked()
                        && let Some(output_path) = rfd::FileDialog::new()
                            .add_filter("MP4", &["mp4"])
                            .set_file_name("trimmed.mp4")
                            .save_file()
                    {
                        let handle = trim::spawn_trim(
                            idx,
                            clip.path.clone(),
                            output_path,
                            clip.in_point.unwrap(),
                            clip.out_point.unwrap(),
                        );
                        self.state.trim_jobs.push(handle);
                    }
                    if ui.button("Export GIF").clicked()
                        && let Some(output_path) = rfd::FileDialog::new()
                            .add_filter("GIF", &["gif"])
                            .set_file_name("preview.gif")
                            .save_file()
                    {
                        let handle = gif::spawn_gif(
                            idx,
                            clip.path.clone(),
                            output_path,
                            clip.in_point,
                            clip.out_point,
                            clip.info.duration(),
                        );
                        self.state.gif_jobs.push(handle);
                    }
                }
            });

        // 4. Center: Source Monitor (must be last)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Source Monitor");
            ui.separator();

            let is_playing = self
                .state
                .player_thread
                .as_ref()
                .map(|h| !h.is_finished())
                .unwrap_or(false);

            // Two control rows when a clip is loaded: seek bar + timecode, then buttons.
            let ctrl_height = if self.state.monitor_clip_index.is_some() {
                72.0
            } else {
                36.0
            };
            let available = ui.available_size();
            let video_size = egui::vec2(available.x, (available.y - ctrl_height).max(0.0));

            if let Some(tex) = &self.state.preview_texture {
                ui.image(egui::load::SizedTexture::new(tex.id(), video_size));
            } else {
                ui.allocate_ui(video_size, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("Double-click a clip to load it");
                    });
                });
            }

            ui.separator();

            // Seek bar + timecode row (only when a clip is loaded).
            if let Some(idx) = self.state.monitor_clip_index {
                let duration_secs = self
                    .state
                    .clips
                    .get(idx)
                    .map(|c| c.info.duration().as_secs_f64())
                    .unwrap_or(1.0)
                    .max(1.0);

                // Sync slider from current PTS while playing.
                // avio API gap: PreviewPlayer has no current_pts() method —
                // we track pts from TimedRgbaSink::push_frame() into
                // AppState::current_pts.
                if is_playing && let Some(pts) = self.state.current_pts {
                    self.state.seek_pos_secs = pts.as_secs_f64().min(duration_secs);
                }

                ui.horizontal(|ui| {
                    let slider_resp = ui.add(
                        egui::Slider::new(&mut self.state.seek_pos_secs, 0.0..=duration_secs)
                            .show_value(false),
                    );

                    // Timecode: HH:MM:SS.mmm
                    let t = self.state.seek_pos_secs;
                    let h = (t / 3600.0) as u64;
                    let m = ((t % 3600.0) / 60.0) as u64;
                    let s = (t % 60.0) as u64;
                    let ms = ((t % 1.0) * 1000.0) as u64;
                    ui.monospace(format!("{h:02}:{m:02}:{s:02}.{ms:03}"));

                    // avio API gap: seek() takes &mut self — cannot call during
                    // run(). Workaround: stop + respawn from the target position.
                    // avio gap: DecodeBuffer::seek_coarse() is not surfaced at
                    // PreviewPlayer level; only exact seek is available.
                    if slider_resp.drag_stopped() {
                        if let Some(stop) = self.state.player_stop.take() {
                            stop.store(true, std::sync::atomic::Ordering::Release);
                        }
                        self.state.player_thread = None;
                        self.state.pending_stop_rx = None;
                        let target = Duration::from_secs_f64(self.state.seek_pos_secs);
                        if let Some(path) = self.state.clips.get(idx).map(|c| c.path.clone()) {
                            let (thread, stop_rx) = player::spawn_player(
                                path,
                                Arc::clone(&self.state.frame_handle),
                                ctx.clone(),
                                Some(target),
                            );
                            self.state.player_thread = Some(thread);
                            self.state.pending_stop_rx = Some(stop_rx);
                        }
                    }
                });
            }

            ui.horizontal(|ui| {
                if is_playing {
                    // avio API gap: pause() takes &mut self so it cannot be called
                    // while run() blocks the player thread. Pause stops playback.
                    if ui.button("⏸ Pause").clicked()
                        && let Some(stop) = self.state.player_stop.take()
                    {
                        stop.store(true, std::sync::atomic::Ordering::Release);
                    }
                    if ui.button("⏹ Stop").clicked()
                        && let Some(stop) = self.state.player_stop.take()
                    {
                        stop.store(true, std::sync::atomic::Ordering::Release);
                    }
                } else if let Some(idx) = self.state.monitor_clip_index {
                    let has_video = self
                        .state
                        .clips
                        .get(idx)
                        .map(|c| c.info.primary_video().is_some())
                        .unwrap_or(false);
                    if has_video
                        && ui.button("▶ Play").clicked()
                        && let Some(path) = self.state.clips.get(idx).map(|c| c.path.clone())
                    {
                        let (thread, stop_rx) = player::spawn_player(
                            path,
                            Arc::clone(&self.state.frame_handle),
                            ctx.clone(),
                            None,
                        );
                        self.state.player_thread = Some(thread);
                        self.state.pending_stop_rx = Some(stop_rx);
                    } else if !has_video {
                        ui.label("No video stream");
                    }
                }
            });
        });
    }
}
