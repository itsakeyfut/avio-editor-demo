use std::sync::Arc;
use std::time::Duration;

use crate::{player, state};

pub fn show(state: &mut state::AppState, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.horizontal(|ui| {
        ui.heading("Source Monitor");
        if state.proxy_active {
            ui.colored_label(egui::Color32::from_rgb(255, 200, 0), "PROXY")
                .on_hover_text("Playing proxy — reload clip to hot-swap");
        }
    });
    ui.separator();

    let is_playing = state
        .player_thread
        .as_ref()
        .map(|h| !h.is_finished())
        .unwrap_or(false);

    // Two control rows when a clip is loaded: seek bar + timecode, then buttons.
    let ctrl_height = if state.monitor_clip_index.is_some() {
        128.0
    } else {
        36.0
    };
    let available = ui.available_size();
    let video_size = egui::vec2(available.x, (available.y - ctrl_height).max(0.0));

    if state.monitor_clip_index.is_some() {
        // Playback mode: show video frame (or "Loading…" while the first frame arrives).
        if let Some(tex) = &state.preview_texture {
            ui.image(egui::load::SizedTexture::new(tex.id(), video_size));
        } else {
            ui.allocate_ui(video_size, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label("Loading…");
                });
            });
        }
    } else if let Some(idx) = state.selected_clip_index
        && let Some(clip) = state.clips.get(idx)
    {
        // Info mode: show MediaInfo for the selected clip.
        ui.allocate_ui(video_size, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("probe_info_scroll")
                .show(ui, |ui| {
                    let file_name = clip.path.file_name().unwrap_or_default().to_string_lossy();
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
    if let Some(idx) = state.monitor_clip_index {
        let duration_secs = state
            .clips
            .get(idx)
            .map(|c| c.info.duration().as_secs_f64())
            .unwrap_or(1.0)
            .max(1.0);

        // Sync slider from current PTS while playing.
        // avio API gap: PreviewPlayer has no current_pts() method —
        // we track pts from TimedRgbaSink::push_frame() into
        // AppState::current_pts.
        if is_playing && let Some(pts) = state.current_pts {
            state.seek_pos_secs = pts.as_secs_f64().min(duration_secs);
        }

        ui.horizontal(|ui| {
            let slider_resp = ui.add(
                egui::Slider::new(&mut state.seek_pos_secs, 0.0..=duration_secs).show_value(false),
            );

            // Draw IN (green) and OUT (orange) markers on the seek bar.
            if let Some(clip) = state.clips.get(idx) {
                let r = slider_resp.rect;
                if let Some(in_pt) = clip.in_point {
                    let x = r.left() + (in_pt.as_secs_f64() / duration_secs) as f32 * r.width();
                    ui.painter().vline(
                        x,
                        r.y_range(),
                        egui::Stroke::new(2.0, egui::Color32::from_rgb(0, 200, 0)),
                    );
                }
                if let Some(out_pt) = clip.out_point {
                    let x = r.left() + (out_pt.as_secs_f64() / duration_secs) as f32 * r.width();
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
                for kf in &state.keyframes {
                    let x = r.left() + (kf.as_secs_f64() / duration_secs) as f32 * r.width();
                    ui.painter().vline(
                        x,
                        r.top()..=(r.top() + 4.0),
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 150, 255)),
                    );
                }
            }

            // Timecode: HH:MM:SS.mmm
            let t = state.seek_pos_secs;
            let h = (t / 3600.0) as u64;
            let m = ((t % 3600.0) / 60.0) as u64;
            let s = (t % 60.0) as u64;
            let ms = ((t % 1.0) * 1000.0) as u64;
            ui.monospace(format!("{h:02}:{m:02}:{s:02}.{ms:03}"));

            // Seek mode toggle.
            // avio API gap: DecodeBuffer::seek_coarse() is not exposed at
            // PreviewPlayer level, so both modes currently use player.seek()
            // (exact). The toggle is wired for when avio surfaces coarse seek.
            let mode_label = if state.seek_exact { "Exact" } else { "Coarse" };
            ui.toggle_value(&mut state.seek_exact, mode_label)
                .on_hover_text("Exact: frame-accurate but slow\nCoarse: nearest keyframe, fast");

            // avio API gap: seek() takes &mut self — cannot call during
            // run(). Workaround: stop + respawn from the target position.
            if slider_resp.drag_stopped() {
                // Snap to nearest keyframe in Coarse mode.
                if !state.seek_exact {
                    state.seek_pos_secs =
                        snap_to_nearest_keyframe(state.seek_pos_secs, &state.keyframes, 0.5);
                }
                if let Some(stop) = state.player_stop.take() {
                    stop.store(true, std::sync::atomic::Ordering::Release);
                }
                state.player_thread = None;
                state.pending_stop_rx = None;
                state.pending_proxy_rx = None;
                let target = Duration::from_secs_f64(state.seek_pos_secs);
                if let Some(path) = state.clips.get(idx).map(|c| c.path.clone()) {
                    let proxy_dir = state
                        .clips
                        .get(idx)
                        .and_then(|c| c.path.parent())
                        .map(|p| p.join("proxies"));
                    let (thread, stop_rx, proxy_rx) = player::spawn_player(
                        path,
                        Arc::clone(&state.frame_handle),
                        ctx.clone(),
                        Some(target),
                        proxy_dir,
                        Arc::clone(&state.rate_handle),
                        state.av_offset_ms as i64,
                    );
                    state.player_thread = Some(thread);
                    state.pending_stop_rx = Some(stop_rx);
                    state.pending_proxy_rx = Some(proxy_rx);
                    state.proxy_active = false;
                }
            }
        });
    }

    ui.horizontal(|ui| {
        if is_playing {
            // avio API gap: pause() takes &mut self so it cannot be called
            // while run() blocks the player thread. Pause stops playback.
            if ui.button("⏸ Pause").clicked()
                && let Some(stop) = state.player_stop.take()
            {
                stop.store(true, std::sync::atomic::Ordering::Release);
                state.proxy_active = false;
            }
            if ui.button("⏹ Stop").clicked()
                && let Some(stop) = state.player_stop.take()
            {
                stop.store(true, std::sync::atomic::Ordering::Release);
                state.proxy_active = false;
            }
        } else if let Some(idx) = state.monitor_clip_index {
            let has_video = state
                .clips
                .get(idx)
                .map(|c| c.info.primary_video().is_some())
                .unwrap_or(false);
            if has_video
                && ui.button("▶ Play").clicked()
                && let Some(path) = state.clips.get(idx).map(|c| c.path.clone())
            {
                let proxy_dir = state
                    .clips
                    .get(idx)
                    .and_then(|c| c.path.parent())
                    .map(|p| p.join("proxies"));
                let (thread, stop_rx, proxy_rx) = player::spawn_player(
                    path,
                    Arc::clone(&state.frame_handle),
                    ctx.clone(),
                    None,
                    proxy_dir,
                    Arc::clone(&state.rate_handle),
                    state.av_offset_ms as i64,
                );
                state.player_thread = Some(thread);
                state.pending_stop_rx = Some(stop_rx);
                state.pending_proxy_rx = Some(proxy_rx);
                state.proxy_active = false;
            } else if !has_video {
                ui.label("No video stream");
            }
        }
        // Rate selector — visible whenever a clip is loaded.
        // avio API gap: PreviewPlayer has no set_rate() — rate is applied
        // inside TimedRgbaSink::push_frame by scaling the sleep duration.
        if state.monitor_clip_index.is_some() {
            ui.separator();
            for (rate, label) in [(0.25_f64, "0.25×"), (0.5, "0.5×"), (1.0, "1×"), (2.0, "2×")]
            {
                if ui
                    .selectable_label(state.playback_rate == rate, label)
                    .clicked()
                {
                    state.playback_rate = rate;
                    state
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
    if let Some(idx) = state.monitor_clip_index {
        ui.horizontal(|ui| {
            ui.label("A/V:");
            let av_resp = ui.add(
                egui::DragValue::new(&mut state.av_offset_ms)
                    .range(-500..=500)
                    .speed(1.0)
                    .suffix(" ms"),
            );
            let should_apply = av_resp.drag_stopped() || (!av_resp.dragged() && av_resp.changed());
            if should_apply && is_playing {
                if let Some(stop) = state.player_stop.take() {
                    stop.store(true, std::sync::atomic::Ordering::Release);
                }
                state.player_thread = None;
                state.pending_stop_rx = None;
                state.pending_proxy_rx = None;
                let target = Duration::from_secs_f64(state.seek_pos_secs);
                if let Some(path) = state.clips.get(idx).map(|c| c.path.clone()) {
                    let proxy_dir = state
                        .clips
                        .get(idx)
                        .and_then(|c| c.path.parent())
                        .map(|p| p.join("proxies"));
                    let (thread, stop_rx, proxy_rx) = player::spawn_player(
                        path,
                        Arc::clone(&state.frame_handle),
                        ctx.clone(),
                        Some(target),
                        proxy_dir,
                        Arc::clone(&state.rate_handle),
                        state.av_offset_ms as i64,
                    );
                    state.player_thread = Some(thread);
                    state.pending_stop_rx = Some(stop_rx);
                    state.pending_proxy_rx = Some(proxy_rx);
                    state.proxy_active = false;
                }
            }
            if ui.small_button("Reset").clicked() {
                state.av_offset_ms = 0;
                if is_playing {
                    if let Some(stop) = state.player_stop.take() {
                        stop.store(true, std::sync::atomic::Ordering::Release);
                    }
                    state.player_thread = None;
                    state.pending_stop_rx = None;
                    state.pending_proxy_rx = None;
                    let target = Duration::from_secs_f64(state.seek_pos_secs);
                    if let Some(path) = state.clips.get(idx).map(|c| c.path.clone()) {
                        let proxy_dir = state
                            .clips
                            .get(idx)
                            .and_then(|c| c.path.parent())
                            .map(|p| p.join("proxies"));
                        let (thread, stop_rx, proxy_rx) = player::spawn_player(
                            path,
                            Arc::clone(&state.frame_handle),
                            ctx.clone(),
                            Some(target),
                            proxy_dir,
                            Arc::clone(&state.rate_handle),
                            0_i64,
                        );
                        state.player_thread = Some(thread);
                        state.pending_stop_rx = Some(stop_rx);
                        state.pending_proxy_rx = Some(proxy_rx);
                        state.proxy_active = false;
                    }
                }
            }
        });
    }

    // IN/OUT marking row — visible whenever a clip is loaded.
    if let Some(idx) = state.monitor_clip_index {
        ui.horizontal(|ui| {
            if ui.small_button("[ Mark In").clicked() {
                let pts = Duration::from_secs_f64(state.seek_pos_secs);
                if let Some(clip) = state.clips.get_mut(idx) {
                    clip.in_point = Some(pts);
                }
            }
            if ui.small_button("Mark Out ]").clicked() {
                let pts = Duration::from_secs_f64(state.seek_pos_secs);
                if let Some(clip) = state.clips.get_mut(idx) {
                    clip.out_point = Some(pts);
                }
            }
            if let Some(clip) = state.clips.get(idx) {
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
                ui.colored_label(egui::Color32::from_rgb(0, 200, 0), format!("IN {in_str}"));
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
}

fn snap_to_nearest_keyframe(
    target_secs: f64,
    keyframes: &[Duration],
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
