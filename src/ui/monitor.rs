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

    let is_active = state
        .player_thread
        .as_ref()
        .map(|h| !h.is_finished())
        .unwrap_or(false);
    let is_paused = state.is_paused;

    let timeline_is_active = state
        .timeline_player_thread
        .as_ref()
        .map(|h| !h.is_finished())
        .unwrap_or(false);

    let ctrl_height = if timeline_is_active {
        0.0
    } else if state.monitor_clip_index.is_some() {
        128.0
    } else {
        36.0
    };
    let available = ui.available_size();
    let video_size = egui::vec2(available.x, (available.y - ctrl_height).max(0.0));

    if timeline_is_active {
        if let Some(tex) = &state.preview_texture {
            let img_rect = show_frame_fitted(ui, tex, video_size);
            draw_title_overlays(state, ui, img_rect);
        } else {
            ui.allocate_ui(video_size, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label("Loading…");
                });
            });
        }
    } else if state.monitor_clip_index.is_some() {
        let is_audio_only = state
            .monitor_clip_index
            .and_then(|idx| state.clips.get(idx))
            .map(|c| c.info.primary_video().is_none() && c.info.primary_audio().is_some())
            .unwrap_or(false);
        if let Some(tex) = &state.preview_texture {
            show_frame_fitted(ui, tex, video_size);
        } else if is_audio_only {
            ui.allocate_ui(video_size, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.heading("\u{1F3B5}");
                });
            });
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

        if is_active && let Some(pts) = state.current_pts {
            state.seek_pos_secs = pts.as_secs_f64().min(duration_secs);
        }

        ui.horizontal(|ui| {
            let slider_resp = ui.add(
                egui::Slider::new(&mut state.seek_pos_secs, 0.0..=duration_secs).show_value(false),
            );

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

            let t = state.seek_pos_secs;
            let h = (t / 3600.0) as u64;
            let m = ((t % 3600.0) / 60.0) as u64;
            let s = (t % 60.0) as u64;
            let ms = ((t % 1.0) * 1000.0) as u64;
            ui.monospace(format!("{h:02}:{m:02}:{s:02}.{ms:03}"));

            let mode_label = if state.seek_exact { "Exact" } else { "Coarse" };
            ui.toggle_value(&mut state.seek_exact, mode_label)
                .on_hover_text("Exact: frame-accurate but slow\nCoarse: nearest keyframe, fast");

            if slider_resp.drag_stopped() {
                if !state.seek_exact {
                    state.seek_pos_secs =
                        snap_to_nearest_keyframe(state.seek_pos_secs, &state.keyframes, 0.5);
                }
                stop_player(state);
                let target = Duration::from_secs_f64(state.seek_pos_secs);
                if let Some(path) = state.clips.get(idx).map(|c| c.path.clone()) {
                    let proxy_dir = state
                        .clips
                        .get(idx)
                        .and_then(|c| c.path.parent())
                        .map(|p| p.join("proxies"));
                    spawn_and_store(state, path, ctx, Some(target), proxy_dir);
                }
            }
        });
    }

    ui.horizontal(|ui| {
        if is_active {
            if is_paused {
                if ui.button("▶ Resume").clicked()
                    && let Some(handle) = &state.player_handle
                {
                    handle.play();
                    state.is_paused = false;
                }
            } else if ui.button("⏸ Pause").clicked()
                && let Some(handle) = &state.player_handle
            {
                handle.pause();
                state.is_paused = true;
            }
            if ui.button("⏹ Stop").clicked() {
                stop_player(state);
                state.monitor_clip_index = None;
                state.seek_pos_secs = 0.0;
            }
        } else if let Some(idx) = state.monitor_clip_index {
            let has_media = state
                .clips
                .get(idx)
                .map(|c| c.info.primary_video().is_some() || c.info.primary_audio().is_some())
                .unwrap_or(false);
            if has_media
                && ui.button("▶ Play").clicked()
                && let Some(path) = state.clips.get(idx).map(|c| c.path.clone())
            {
                let proxy_dir = state
                    .clips
                    .get(idx)
                    .and_then(|c| c.path.parent())
                    .map(|p| p.join("proxies"));
                spawn_and_store(state, path, ctx, None, proxy_dir);
            }
        }

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
                        .cpal_rate
                        .store(rate.to_bits(), std::sync::atomic::Ordering::Relaxed);
                    if let Some(handle) = &state.player_handle {
                        handle.set_rate(rate);
                    }
                }
            }
        }
    });

    // A/V offset row — live-updates via PlayerHandle::set_av_offset (no stop+respawn needed).
    if state.monitor_clip_index.is_some() {
        ui.horizontal(|ui| {
            ui.label("A/V:");
            let av_resp = ui.add(
                egui::DragValue::new(&mut state.av_offset_ms)
                    .range(-500..=500)
                    .speed(1.0)
                    .suffix(" ms"),
            );
            let should_apply = av_resp.drag_stopped() || (!av_resp.dragged() && av_resp.changed());
            if should_apply && let Some(handle) = &state.player_handle {
                handle.set_av_offset(state.av_offset_ms as i64);
            }
            if ui.small_button("Reset").clicked() {
                state.av_offset_ms = 0;
                if let Some(handle) = &state.player_handle {
                    handle.set_av_offset(0);
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
                if let (Some(i), Some(o)) = (clip.in_point, clip.out_point)
                    && i >= o
                {
                    ui.colored_label(egui::Color32::RED, "!");
                }
            }
        });
    }
}

/// Stops the active player and clears all player-related state.
fn stop_player(state: &mut state::AppState) {
    if let Some(handle) = state.player_handle.take() {
        handle.stop();
    }
    state.player_thread = None;
    state.pending_handle_rx = None;
    state.pending_proxy_rx = None;
    state.is_paused = false;
    state.proxy_active = false;
}

/// Spawns a new player and stores all resulting handles in `state`.
fn spawn_and_store(
    state: &mut state::AppState,
    path: std::path::PathBuf,
    ctx: &egui::Context,
    start_pos: Option<Duration>,
    proxy_dir: Option<std::path::PathBuf>,
) {
    state.cpal_rate.store(
        state.playback_rate.to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
    let (thread, handle_rx, proxy_rx) = player::spawn_player(
        path,
        Arc::clone(&state.frame_handle),
        ctx.clone(),
        start_pos,
        proxy_dir,
        state.playback_rate,
        state.av_offset_ms as i64,
        Arc::clone(&state.cpal_rate),
    );
    state.player_thread = Some(thread);
    state.pending_handle_rx = Some(handle_rx);
    state.pending_proxy_rx = Some(proxy_rx);
    state.proxy_active = false;
    state.is_paused = false;
}

/// Paints the preview texture inside `video_size`, preserving the frame's aspect
/// ratio (letterbox/pillarbox within the area) rather than stretching to fill it —
/// so a 9:16 (or 1:1, 4:5) project shows its true shape. Returns the rect the image
/// was painted into (used to anchor title overlays).
fn show_frame_fitted(
    ui: &mut egui::Ui,
    tex: &egui::TextureHandle,
    video_size: egui::Vec2,
) -> egui::Rect {
    let (area, _) = ui.allocate_exact_size(video_size, egui::Sense::hover());
    let tex_size = tex.size_vec2();
    if tex_size.x <= 0.0 || tex_size.y <= 0.0 || video_size.x <= 0.0 || video_size.y <= 0.0 {
        return area;
    }
    let scale = (video_size.x / tex_size.x).min(video_size.y / tex_size.y);
    let disp = tex_size * scale;
    let img_rect = egui::Rect::from_center_size(area.center(), disp);
    // Fill the frame rect with black before painting the (possibly alpha-carrying)
    // frame, so transparent areas — e.g. from a chroma key / mask on a base clip —
    // composite over black, matching the export's black canvas. Without this the
    // egui panel background shows through and preview ≠ export.
    ui.painter()
        .rect_filled(img_rect, 0.0, egui::Color32::BLACK);
    egui::Image::new(egui::load::SizedTexture::new(tex.id(), disp)).paint_at(ui, img_rect);
    img_rect
}

/// Draws active T1 title clips as egui text on top of the preview image rect.
fn draw_title_overlays(state: &state::AppState, ui: &egui::Ui, img_rect: egui::Rect) {
    let t = state.timeline_playhead_secs;
    let painter = ui.painter();

    for tc in &state.timeline.title_clips {
        if tc.text.is_empty() {
            continue;
        }
        let start = tc.start_on_track.as_secs_f64();
        let end = start + tc.duration.as_secs_f64();
        if t < start || t >= end {
            continue;
        }

        let [r, g, b, a] = tc.color;
        let color = egui::Color32::from_rgba_unmultiplied(r, g, b, a);

        // Scale font proportionally to preview width (assumes 1920-wide reference).
        let font_size = (tc.font_size as f32 * img_rect.width() / 1920.0).max(8.0);

        let pos = match (tc.h_align, tc.v_align) {
            (state::HAlign::Left, state::VAlign::Top) => {
                egui::pos2(img_rect.left() + 10.0, img_rect.top() + 10.0)
            }
            (state::HAlign::Left, state::VAlign::Middle) => {
                egui::pos2(img_rect.left() + 10.0, img_rect.center().y)
            }
            (state::HAlign::Left, state::VAlign::Bottom) => {
                egui::pos2(img_rect.left() + 10.0, img_rect.bottom() - 10.0)
            }
            (state::HAlign::Centre, state::VAlign::Top) => {
                egui::pos2(img_rect.center().x, img_rect.top() + 10.0)
            }
            (state::HAlign::Centre, state::VAlign::Middle) => img_rect.center(),
            (state::HAlign::Centre, state::VAlign::Bottom) => {
                egui::pos2(img_rect.center().x, img_rect.bottom() - 10.0)
            }
            (state::HAlign::Right, state::VAlign::Top) => {
                egui::pos2(img_rect.right() - 10.0, img_rect.top() + 10.0)
            }
            (state::HAlign::Right, state::VAlign::Middle) => {
                egui::pos2(img_rect.right() - 10.0, img_rect.center().y)
            }
            (state::HAlign::Right, state::VAlign::Bottom) => {
                egui::pos2(img_rect.right() - 10.0, img_rect.bottom() - 10.0)
            }
        };

        let halign = match tc.h_align {
            state::HAlign::Left => egui::Align2::LEFT_CENTER,
            state::HAlign::Centre => egui::Align2::CENTER_CENTER,
            state::HAlign::Right => egui::Align2::RIGHT_CENTER,
        };
        let valign = match tc.v_align {
            state::VAlign::Top => egui::Align2::CENTER_TOP,
            state::VAlign::Middle => egui::Align2::CENTER_CENTER,
            state::VAlign::Bottom => egui::Align2::CENTER_BOTTOM,
        };
        // Combine h and v alignment into a single Align2.
        let align = egui::Align2([halign.x(), valign.y()]);

        painter.text(
            pos,
            align,
            &tc.text,
            egui::FontId::proportional(font_size),
            color,
        );
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
