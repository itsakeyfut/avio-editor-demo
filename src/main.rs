mod analysis;
mod gif;
mod player;
mod proxy;
mod state;
mod thumbnail;
mod trim;
use std::sync::Arc;
use std::time::Duration;

use state::{AppState, GifStatus, ImportedClip, ProxyStatus, TrimStatus};

fn snap_to_nearest_keyframe(
    target_secs: f64,
    keyframes: &[std::time::Duration],
    snap_radius_secs: f64,
) -> f64 {
    keyframes
        .iter()
        .map(|kf| kf.as_secs_f64())
        .min_by(|a, b| {
            let da = (a - target_secs).abs();
            let db = (b - target_secs).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .filter(|&nearest| (nearest - target_secs).abs() <= snap_radius_secs)
        .unwrap_or(target_secs)
}

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
                        silence_regions: Vec::new(),
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
                        let path_for_scene = path.clone();
                        tokio::task::spawn_blocking(move || {
                            let scenes = analysis::detect_scenes(&path_for_scene);
                            let _ = scene_tx.send((clip_idx, scenes));
                        });
                    }
                    let silence_tx = self.state.silence_tx.clone();
                    let path_for_silence = path.clone();
                    tokio::task::spawn_blocking(move || {
                        let regions = analysis::detect_silence(&path_for_silence);
                        let _ = silence_tx.send((clip_idx, regions));
                    });
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

        // Drain completed proxy jobs each frame.
        let mut proxy_done: Vec<(usize, std::path::PathBuf)> = Vec::new();
        self.state.proxy_jobs.retain(|job| {
            match job
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                ProxyStatus::Running => true,
                ProxyStatus::Done(path) => {
                    proxy_done.push((job.clip_index, path));
                    false
                }
                ProxyStatus::Failed(_) => true, // kept to display error badge
            }
        });
        for (clip_idx, path) in proxy_done {
            if let Some(clip) = self.state.clips.get_mut(clip_idx) {
                clip.proxy_path = Some(path);
            }
        }

        // Receive stop handle from a freshly spawned player thread.
        if let Some(rx) = &self.state.pending_stop_rx
            && let Ok(stop) = rx.try_recv()
        {
            self.state.player_stop = Some(stop);
            self.state.pending_stop_rx = None;
        }

        // Drain proxy-active status from a freshly spawned player thread.
        if let Some(rx) = &self.state.pending_proxy_rx
            && let Ok(active) = rx.try_recv()
        {
            self.state.proxy_active = active;
            self.state.pending_proxy_rx = None;
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
        while let Ok((idx, regions)) = self.state.silence_rx.try_recv() {
            if let Some(clip) = self.state.clips.get_mut(idx) {
                clip.silence_regions = regions;
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

        // Drain keyframe enumeration results for the currently loaded clip.
        if let Ok(kfs) = self.state.keyframe_rx.try_recv() {
            self.state.keyframes = kfs;
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

                const TRACK_HEIGHT: f32 = 40.0;
                const LABEL_WIDTH: f32 = 40.0;

                let pps = self.state.timeline.pixels_per_second;

                // Dynamic content width: max clip end-time × pps + 200 px padding, min 1200 px.
                let max_end_secs = self
                    .state
                    .timeline
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .filter_map(|tc| {
                        self.state.clips.get(tc.source_index).map(|c| {
                            let dur = match (tc.in_point, tc.out_point) {
                                (Some(i), Some(o)) if o > i => o - i,
                                _ => c.info.duration(),
                            };
                            tc.start_on_track.as_secs_f32() + dur.as_secs_f32()
                        })
                    })
                    .fold(0.0f32, f32::max);
                let content_width = (max_end_secs * pps + 200.0).max(1200.0);

                let mut pending_clips: Vec<(usize, usize, f32)> = Vec::new();

                egui::ScrollArea::horizontal()
                    .id_salt("timeline_scroll")
                    .show(ui, |ui| {
                        // ── Ruler ──────────────────────────────────────────────
                        let (_, ruler_rect) = ui.allocate_space(egui::vec2(content_width, 24.0));
                        let painter = ui.painter_at(ruler_rect);
                        painter.rect_filled(ruler_rect, 0.0, egui::Color32::from_gray(40));

                        // Time tick marks every 5 s
                        let mut t = 0.0f32;
                        while t * pps < content_width {
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
                                    let x = ruler_rect.left()
                                        + (tc.start_on_track + scene_ts).as_secs_f32() * pps;
                                    if x >= ruler_rect.left() && x <= ruler_rect.right() {
                                        painter.vline(
                                            x,
                                            ruler_rect.y_range(),
                                            egui::Stroke::new(
                                                1.0,
                                                egui::Color32::from_rgb(255, 165, 0),
                                            ),
                                        );
                                    }
                                }
                            }
                        }

                        // ── Track lanes ────────────────────────────────────────
                        for (track_idx, track) in self.state.timeline.tracks.iter().enumerate() {
                            ui.horizontal(|ui| {
                                // Track label
                                ui.allocate_ui_with_layout(
                                    egui::vec2(LABEL_WIDTH, TRACK_HEIGHT),
                                    egui::Layout::centered_and_justified(
                                        egui::Direction::LeftToRight,
                                    ),
                                    |ui| {
                                        ui.label(match track.kind {
                                            state::TrackKind::Video1 => "V1",
                                            state::TrackKind::Video2 => "V2",
                                            state::TrackKind::Audio1 => "A1",
                                        });
                                    },
                                );

                                // Lane drop zone
                                let (lane_rect, lane_resp) = ui.allocate_exact_size(
                                    egui::vec2(content_width - LABEL_WIDTH, TRACK_HEIGHT),
                                    egui::Sense::hover(),
                                );

                                // Lane background — highlight when a clip is dragged over
                                let bg = if lane_resp.dnd_hover_payload::<usize>().is_some() {
                                    egui::Color32::from_gray(55)
                                } else {
                                    egui::Color32::from_gray(35)
                                };
                                ui.painter().rect_filled(lane_rect, 0.0, bg);
                                ui.painter().rect_stroke(
                                    lane_rect,
                                    0.0,
                                    egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
                                    egui::StrokeKind::Inside,
                                );

                                // Clip rectangles
                                let clip_color = match track.kind {
                                    state::TrackKind::Video1 | state::TrackKind::Video2 => {
                                        egui::Color32::from_rgb(70, 130, 180) // steel blue
                                    }
                                    state::TrackKind::Audio1 => {
                                        egui::Color32::from_rgb(70, 150, 120) // teal
                                    }
                                };
                                for tc in &track.clips {
                                    if let Some(source) = self.state.clips.get(tc.source_index) {
                                        let eff_dur = match (tc.in_point, tc.out_point) {
                                            (Some(i), Some(o)) if o > i => o - i,
                                            _ => source.info.duration(),
                                        };
                                        let x = lane_rect.left()
                                            + tc.start_on_track.as_secs_f32() * pps;
                                        let w = eff_dur.as_secs_f32() * pps;
                                        let cr = egui::Rect::from_min_size(
                                            egui::pos2(x, lane_rect.top()),
                                            egui::vec2(w.max(2.0), TRACK_HEIGHT),
                                        );
                                        if cr.max.x >= lane_rect.left()
                                            && cr.min.x <= lane_rect.right()
                                        {
                                            ui.painter().rect_filled(cr, 4.0, clip_color);
                                            let name = source
                                                .path
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy();
                                            ui.painter().text(
                                                cr.left_center() + egui::vec2(4.0, 0.0),
                                                egui::Align2::LEFT_CENTER,
                                                name.as_ref(),
                                                egui::FontId::proportional(11.0),
                                                egui::Color32::WHITE,
                                            );
                                            // Silence region overlays — A1 track only
                                            if track.kind == state::TrackKind::Audio1 {
                                                for &(start, end) in &source.silence_regions {
                                                    let sx0 = lane_rect.left()
                                                        + (tc.start_on_track + start).as_secs_f32()
                                                            * pps;
                                                    let sx1 = lane_rect.left()
                                                        + (tc.start_on_track + end).as_secs_f32()
                                                            * pps;
                                                    let sr = egui::Rect::from_x_y_ranges(
                                                        sx0..=sx1,
                                                        lane_rect.y_range(),
                                                    )
                                                    .intersect(cr);
                                                    if sr.is_positive() {
                                                        ui.painter().rect_filled(
                                                            sr,
                                                            0.0,
                                                            egui::Color32::from_rgba_premultiplied(
                                                                0, 0, 0, 100,
                                                            ),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }

                                // Drop handling
                                if let Some(clip_idx_arc) = lane_resp.dnd_release_payload::<usize>()
                                {
                                    let ptr_x = ui.input(|i| {
                                        i.pointer
                                            .latest_pos()
                                            .map(|p| p.x)
                                            .unwrap_or(lane_rect.left())
                                    });
                                    let start_secs = ((ptr_x - lane_rect.left()) / pps).max(0.0);
                                    pending_clips.push((track_idx, *clip_idx_arc, start_secs));
                                }
                            });
                        }
                    }); // end ScrollArea

                // Apply drops after the ScrollArea closure to avoid borrow conflicts.
                for (track_idx, clip_idx, start_secs) in pending_clips {
                    let (in_pt, out_pt) = self
                        .state
                        .clips
                        .get(clip_idx)
                        .map(|c| (c.in_point, c.out_point))
                        .unwrap_or_default();
                    self.state.timeline.tracks[track_idx]
                        .clips
                        .push(state::TimelineClip {
                            source_index: clip_idx,
                            start_on_track: Duration::from_secs_f32(start_secs),
                            in_point: in_pt,
                            out_point: out_pt,
                        });
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
                                    silence_regions: Vec::new(),
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
                                let silence_tx = self.state.silence_tx.clone();
                                let path_for_silence = path.clone();
                                tokio::task::spawn_blocking(move || {
                                    let regions = analysis::detect_silence(&path_for_silence);
                                    let _ = silence_tx.send((clip_idx, regions));
                                });
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
                        let dnd_id = egui::Id::new(("clip_dnd", idx));
                        let is_dragging = ui.ctx().is_being_dragged(dnd_id);
                        let dnd = ui.dnd_drag_source(dnd_id, idx, |ui| {
                            ui.selectable_label(selected, name.as_ref())
                        });
                        // dnd_drag_source adds CursorIcon::Grab on hover.
                        // Override: show Grabbing only while actively dragging,
                        // Default cursor otherwise.
                        if is_dragging {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        } else if dnd.response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                        }
                        if dnd.inner.clicked() {
                            clicked_idx = Some(idx);
                        }
                        if dnd.inner.double_clicked() {
                            dbl_clicked_idx = Some(idx);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(clip.duration_label());
                            if clip.proxy_path.is_some() {
                                ui.colored_label(egui::Color32::from_rgb(0, 200, 0), "Proxy");
                            } else {
                                let job_status = self
                                    .state
                                    .proxy_jobs
                                    .iter()
                                    .find(|j| j.clip_index == idx)
                                    .map(|j| {
                                        j.status
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                                            .clone()
                                    });
                                match job_status {
                                    Some(ProxyStatus::Running) => {
                                        ui.spinner();
                                    }
                                    Some(ProxyStatus::Failed(ref msg)) => {
                                        ui.colored_label(egui::Color32::RED, "Failed")
                                            .on_hover_text(msg.as_str());
                                    }
                                    _ => {}
                                }
                            }
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
                        // Clear stale keyframes and enumerate new ones for this clip.
                        self.state.keyframes.clear();
                        let kf_tx = self.state.keyframe_tx.clone();
                        let kf_path = path.clone();
                        tokio::task::spawn_blocking(move || {
                            let kfs = analysis::enumerate_keyframes(&kf_path);
                            let _ = kf_tx.send(kfs);
                        });
                        let proxy_dir = self
                            .state
                            .clips
                            .get(idx)
                            .and_then(|c| c.path.parent())
                            .map(|p| p.join("proxies"));
                        let (thread, stop_rx, proxy_rx) = player::spawn_player(
                            path,
                            Arc::clone(&self.state.frame_handle),
                            ctx.clone(),
                            None,
                            proxy_dir,
                            Arc::clone(&self.state.rate_handle),
                            self.state.av_offset_ms as i64,
                        );
                        self.state.player_thread = Some(thread);
                        self.state.pending_stop_rx = Some(stop_rx);
                        self.state.pending_proxy_rx = Some(proxy_rx);
                        self.state.proxy_active = false;
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
                    let is_proxy_running = self.state.proxy_jobs.iter().any(|j| {
                        j.clip_index == idx
                            && matches!(
                                j.status
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .clone(),
                                ProxyStatus::Running
                            )
                    });
                    if ui
                        .add_enabled(!is_proxy_running, egui::Button::new("Gen Proxy"))
                        .clicked()
                    {
                        // Remove any stale job for this clip before starting a new one.
                        self.state.proxy_jobs.retain(|j| j.clip_index != idx);
                        if let Some(c) = self.state.clips.get(idx) {
                            let proxy_dir = c
                                .path
                                .parent()
                                .map(|p| p.join("proxies"))
                                .unwrap_or_default();
                            if let Err(e) = std::fs::create_dir_all(&proxy_dir) {
                                log::warn!("failed to create proxy dir {proxy_dir:?}: {e}");
                            }
                            let handle = proxy::spawn_proxy_job(idx, c.path.clone(), proxy_dir);
                            self.state.proxy_jobs.push(handle);
                        }
                    }
                    if ui.button("Add to V1").clicked() {
                        let start = self.state.timeline.tracks[0]
                            .clips
                            .last()
                            .map(|tc| {
                                let effective = match (tc.in_point, tc.out_point) {
                                    (Some(i), Some(o)) if o > i => o - i,
                                    _ => self.state.clips[tc.source_index].info.duration(),
                                };
                                tc.start_on_track + effective
                            })
                            .unwrap_or_default();
                        let (tc_in, tc_out) = self
                            .state
                            .clips
                            .get(idx)
                            .map(|c| (c.in_point, c.out_point))
                            .unwrap_or_default();
                        self.state.timeline.tracks[0]
                            .clips
                            .push(state::TimelineClip {
                                source_index: idx,
                                start_on_track: start,
                                in_point: tc_in,
                                out_point: tc_out,
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
            ui.horizontal(|ui| {
                ui.heading("Source Monitor");
                if self.state.proxy_active {
                    ui.colored_label(egui::Color32::from_rgb(255, 200, 0), "PROXY")
                        .on_hover_text("Playing proxy — reload clip to hot-swap");
                }
            });
            ui.separator();

            let is_playing = self
                .state
                .player_thread
                .as_ref()
                .map(|h| !h.is_finished())
                .unwrap_or(false);

            // Two control rows when a clip is loaded: seek bar + timecode, then buttons.
            let ctrl_height = if self.state.monitor_clip_index.is_some() {
                128.0
            } else {
                36.0
            };
            let available = ui.available_size();
            let video_size = egui::vec2(available.x, (available.y - ctrl_height).max(0.0));

            if self.state.monitor_clip_index.is_some() {
                // Playback mode: show video frame (or "Loading…" while the first frame arrives).
                if let Some(tex) = &self.state.preview_texture {
                    ui.image(egui::load::SizedTexture::new(tex.id(), video_size));
                } else {
                    ui.allocate_ui(video_size, |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.label("Loading…");
                        });
                    });
                }
            } else if let Some(idx) = self.state.selected_clip_index
                && let Some(clip) = self.state.clips.get(idx)
            {
                // Info mode: show MediaInfo for the selected clip.
                ui.allocate_ui(video_size, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("probe_info_scroll")
                        .show(ui, |ui| {
                            let file_name =
                                clip.path.file_name().unwrap_or_default().to_string_lossy();
                            ui.heading(file_name.as_ref());
                            ui.separator();
                            let info = &clip.info;
                            egui::Grid::new("probe_info_grid")
                                .num_columns(2)
                                .spacing([12.0, 4.0])
                                .show(ui, |ui| {
                                    ui.strong("Container:");
                                    ui.label(info.format());
                                    ui.end_row();

                                    let d = info.duration();
                                    let total_secs = d.as_secs();
                                    ui.strong("Duration:");
                                    ui.label(format!(
                                        "{}:{:02}.{:03}",
                                        total_secs / 60,
                                        total_secs % 60,
                                        d.subsec_millis()
                                    ));
                                    ui.end_row();

                                    let size_mb = info.file_size() as f64 / 1_000_000.0;
                                    ui.strong("File size:");
                                    ui.label(format!("{size_mb:.1} MB"));
                                    ui.end_row();

                                    if let Some(bps) = info.bitrate() {
                                        ui.strong("Bitrate:");
                                        ui.label(format!("{} kb/s", bps / 1000));
                                        ui.end_row();
                                    }

                                    if let Some(v) = info.primary_video() {
                                        ui.strong("Video:");
                                        ui.label(format!(
                                            "{} {}×{} {:.3} fps",
                                            v.codec().display_name(),
                                            v.width(),
                                            v.height(),
                                            v.fps()
                                        ));
                                        ui.end_row();

                                        if let Some(br) = v.bitrate() {
                                            ui.strong("V-bitrate:");
                                            ui.label(format!("{} kb/s", br / 1000));
                                            ui.end_row();
                                        }
                                    }

                                    if let Some(a) = info.primary_audio() {
                                        ui.strong("Audio:");
                                        ui.label(format!(
                                            "{} {} Hz {}ch",
                                            a.codec().display_name(),
                                            a.sample_rate(),
                                            a.channels()
                                        ));
                                        ui.end_row();

                                        if let Some(br) = a.bitrate() {
                                            ui.strong("A-bitrate:");
                                            ui.label(format!("{} kb/s", br / 1000));
                                            ui.end_row();
                                        }
                                    }
                                });
                        });
                });
            } else {
                ui.allocate_ui(video_size, |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label("Click to view clip info · Double-click to play");
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

                    // Draw IN (green) and OUT (orange) markers on the seek bar.
                    if let Some(clip) = self.state.clips.get(idx) {
                        let r = slider_resp.rect;
                        if let Some(in_pt) = clip.in_point {
                            let x =
                                r.left() + (in_pt.as_secs_f64() / duration_secs) as f32 * r.width();
                            ui.painter().vline(
                                x,
                                r.y_range(),
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(0, 200, 0)),
                            );
                        }
                        if let Some(out_pt) = clip.out_point {
                            let x = r.left()
                                + (out_pt.as_secs_f64() / duration_secs) as f32 * r.width();
                            ui.painter().vline(
                                x,
                                r.y_range(),
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 140, 0)),
                            );
                        }
                    }

                    // Draw keyframe tick marks (blue, 4 px tall) above the seek bar.
                    {
                        let r = slider_resp.rect;
                        for kf in &self.state.keyframes {
                            let x =
                                r.left() + (kf.as_secs_f64() / duration_secs) as f32 * r.width();
                            ui.painter().vline(
                                x,
                                r.top()..=(r.top() + 4.0),
                                egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 150, 255)),
                            );
                        }
                    }

                    // Timecode: HH:MM:SS.mmm
                    let t = self.state.seek_pos_secs;
                    let h = (t / 3600.0) as u64;
                    let m = ((t % 3600.0) / 60.0) as u64;
                    let s = (t % 60.0) as u64;
                    let ms = ((t % 1.0) * 1000.0) as u64;
                    ui.monospace(format!("{h:02}:{m:02}:{s:02}.{ms:03}"));

                    // Seek mode toggle.
                    // avio API gap: DecodeBuffer::seek_coarse() is not exposed at
                    // PreviewPlayer level, so both modes currently use player.seek()
                    // (exact). The toggle is wired for when avio surfaces coarse seek.
                    let mode_label = if self.state.seek_exact {
                        "Exact"
                    } else {
                        "Coarse"
                    };
                    ui.toggle_value(&mut self.state.seek_exact, mode_label)
                        .on_hover_text(
                            "Exact: frame-accurate but slow\nCoarse: nearest keyframe, fast",
                        );

                    // avio API gap: seek() takes &mut self — cannot call during
                    // run(). Workaround: stop + respawn from the target position.
                    if slider_resp.drag_stopped() {
                        // Snap to nearest keyframe in Coarse mode.
                        if !self.state.seek_exact {
                            self.state.seek_pos_secs = snap_to_nearest_keyframe(
                                self.state.seek_pos_secs,
                                &self.state.keyframes,
                                0.5,
                            );
                        }
                        if let Some(stop) = self.state.player_stop.take() {
                            stop.store(true, std::sync::atomic::Ordering::Release);
                        }
                        self.state.player_thread = None;
                        self.state.pending_stop_rx = None;
                        self.state.pending_proxy_rx = None;
                        let target = Duration::from_secs_f64(self.state.seek_pos_secs);
                        if let Some(path) = self.state.clips.get(idx).map(|c| c.path.clone()) {
                            let proxy_dir = self
                                .state
                                .clips
                                .get(idx)
                                .and_then(|c| c.path.parent())
                                .map(|p| p.join("proxies"));
                            let (thread, stop_rx, proxy_rx) = player::spawn_player(
                                path,
                                Arc::clone(&self.state.frame_handle),
                                ctx.clone(),
                                Some(target),
                                proxy_dir,
                                Arc::clone(&self.state.rate_handle),
                                self.state.av_offset_ms as i64,
                            );
                            self.state.player_thread = Some(thread);
                            self.state.pending_stop_rx = Some(stop_rx);
                            self.state.pending_proxy_rx = Some(proxy_rx);
                            self.state.proxy_active = false;
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
                        self.state.proxy_active = false;
                    }
                    if ui.button("⏹ Stop").clicked()
                        && let Some(stop) = self.state.player_stop.take()
                    {
                        stop.store(true, std::sync::atomic::Ordering::Release);
                        self.state.proxy_active = false;
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
                        let proxy_dir = self
                            .state
                            .clips
                            .get(idx)
                            .and_then(|c| c.path.parent())
                            .map(|p| p.join("proxies"));
                        let (thread, stop_rx, proxy_rx) = player::spawn_player(
                            path,
                            Arc::clone(&self.state.frame_handle),
                            ctx.clone(),
                            None,
                            proxy_dir,
                            Arc::clone(&self.state.rate_handle),
                            self.state.av_offset_ms as i64,
                        );
                        self.state.player_thread = Some(thread);
                        self.state.pending_stop_rx = Some(stop_rx);
                        self.state.pending_proxy_rx = Some(proxy_rx);
                        self.state.proxy_active = false;
                    } else if !has_video {
                        ui.label("No video stream");
                    }
                }
                // Rate selector — visible whenever a clip is loaded.
                // avio API gap: PreviewPlayer has no set_rate() — rate is applied
                // inside TimedRgbaSink::push_frame by scaling the sleep duration.
                if self.state.monitor_clip_index.is_some() {
                    ui.separator();
                    for (rate, label) in
                        [(0.25_f64, "0.25×"), (0.5, "0.5×"), (1.0, "1×"), (2.0, "2×")]
                    {
                        if ui
                            .selectable_label(self.state.playback_rate == rate, label)
                            .clicked()
                        {
                            self.state.playback_rate = rate;
                            self.state
                                .rate_handle
                                .store(rate.to_bits(), std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            });

            // A/V offset row — visible whenever a clip is loaded.
            // avio API gap: set_av_offset(&self) uses AtomicI64 (thread-safe)
            // but there is no av_offset_handle() method analogous to
            // stop_handle(). Without a handle the UI thread cannot write to
            // the player while run() holds &mut self on the player thread.
            // Workaround: stop + respawn at current position on drag release.
            if let Some(idx) = self.state.monitor_clip_index {
                ui.horizontal(|ui| {
                    ui.label("A/V:");
                    let av_resp = ui.add(
                        egui::DragValue::new(&mut self.state.av_offset_ms)
                            .range(-500..=500)
                            .speed(1.0)
                            .suffix(" ms"),
                    );
                    let should_apply =
                        av_resp.drag_stopped() || (!av_resp.dragged() && av_resp.changed());
                    if should_apply && is_playing {
                        if let Some(stop) = self.state.player_stop.take() {
                            stop.store(true, std::sync::atomic::Ordering::Release);
                        }
                        self.state.player_thread = None;
                        self.state.pending_stop_rx = None;
                        self.state.pending_proxy_rx = None;
                        let target = Duration::from_secs_f64(self.state.seek_pos_secs);
                        if let Some(path) = self.state.clips.get(idx).map(|c| c.path.clone()) {
                            let proxy_dir = self
                                .state
                                .clips
                                .get(idx)
                                .and_then(|c| c.path.parent())
                                .map(|p| p.join("proxies"));
                            let (thread, stop_rx, proxy_rx) = player::spawn_player(
                                path,
                                Arc::clone(&self.state.frame_handle),
                                ctx.clone(),
                                Some(target),
                                proxy_dir,
                                Arc::clone(&self.state.rate_handle),
                                self.state.av_offset_ms as i64,
                            );
                            self.state.player_thread = Some(thread);
                            self.state.pending_stop_rx = Some(stop_rx);
                            self.state.pending_proxy_rx = Some(proxy_rx);
                            self.state.proxy_active = false;
                        }
                    }
                    if ui.small_button("Reset").clicked() {
                        self.state.av_offset_ms = 0;
                        if is_playing {
                            if let Some(stop) = self.state.player_stop.take() {
                                stop.store(true, std::sync::atomic::Ordering::Release);
                            }
                            self.state.player_thread = None;
                            self.state.pending_stop_rx = None;
                            self.state.pending_proxy_rx = None;
                            let target = Duration::from_secs_f64(self.state.seek_pos_secs);
                            if let Some(path) = self.state.clips.get(idx).map(|c| c.path.clone()) {
                                let proxy_dir = self
                                    .state
                                    .clips
                                    .get(idx)
                                    .and_then(|c| c.path.parent())
                                    .map(|p| p.join("proxies"));
                                let (thread, stop_rx, proxy_rx) = player::spawn_player(
                                    path,
                                    Arc::clone(&self.state.frame_handle),
                                    ctx.clone(),
                                    Some(target),
                                    proxy_dir,
                                    Arc::clone(&self.state.rate_handle),
                                    0_i64,
                                );
                                self.state.player_thread = Some(thread);
                                self.state.pending_stop_rx = Some(stop_rx);
                                self.state.pending_proxy_rx = Some(proxy_rx);
                                self.state.proxy_active = false;
                            }
                        }
                    }
                });
            }

            // IN/OUT marking row — visible whenever a clip is loaded.
            if let Some(idx) = self.state.monitor_clip_index {
                ui.horizontal(|ui| {
                    if ui.small_button("[ Mark In").clicked() {
                        let pts = Duration::from_secs_f64(self.state.seek_pos_secs);
                        if let Some(clip) = self.state.clips.get_mut(idx) {
                            clip.in_point = Some(pts);
                        }
                    }
                    if ui.small_button("Mark Out ]").clicked() {
                        let pts = Duration::from_secs_f64(self.state.seek_pos_secs);
                        if let Some(clip) = self.state.clips.get_mut(idx) {
                            clip.out_point = Some(pts);
                        }
                    }
                    if let Some(clip) = self.state.clips.get(idx) {
                        let fmt_tc = |d: Duration| {
                            let t = d.as_secs_f64();
                            let h = (t / 3600.0) as u64;
                            let m = ((t % 3600.0) / 60.0) as u64;
                            let s = (t % 60.0) as u64;
                            let ms = ((t % 1.0) * 1000.0) as u64;
                            format!("{h:02}:{m:02}:{s:02}.{ms:03}")
                        };
                        let in_str = clip
                            .in_point
                            .map(fmt_tc)
                            .unwrap_or_else(|| "\u{2014}".into());
                        let out_str = clip
                            .out_point
                            .map(fmt_tc)
                            .unwrap_or_else(|| "\u{2014}".into());
                        ui.colored_label(
                            egui::Color32::from_rgb(0, 200, 0),
                            format!("IN {in_str}"),
                        );
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 140, 0),
                            format!("OUT {out_str}"),
                        );
                        // Warn if in_point ≥ out_point (invalid range).
                        if let (Some(i), Some(o)) = (clip.in_point, clip.out_point)
                            && i >= o
                        {
                            ui.colored_label(egui::Color32::RED, "!");
                        }
                    }
                });
            }
        });
    }
}
