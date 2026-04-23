use std::sync::Arc;
use std::time::Duration;

use crate::presets::PresetFile;
use crate::{export, player, state};

pub fn show(state: &mut state::AppState, ui: &mut egui::Ui) {
    // Header: "Timeline" heading + Export button aligned right
    let mut clear_export = false;
    ui.horizontal(|ui| {
        ui.heading("Timeline");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let v1_empty = state.timeline.tracks[0].clips.is_empty();
            let is_running = state
                .export
                .as_ref()
                .is_some_and(|h| matches!(*h.status.lock().unwrap(), state::ExportStatus::Running));
            if ui
                .add_enabled(!v1_empty && !is_running, egui::Button::new("Export"))
                .clicked()
                && let Some(output_path) = rfd::FileDialog::new()
                    .add_filter("MP4", &["mp4"])
                    .set_file_name("export.mp4")
                    .save_file()
            {
                let clips = &state.clips;
                let make_clip = |tc: &state::TimelineClip| export::ExportClip {
                    path: clips[tc.source_index].path.clone(),
                    start_on_track: tc.start_on_track,
                    in_point: tc.in_point,
                    out_point: tc.out_point,
                    transition: tc.transition,
                    transition_duration: tc.transition_duration,
                };
                let snapshot = export::ExportSnapshot {
                    v1_clips: state.timeline.tracks[0]
                        .clips
                        .iter()
                        .map(make_clip)
                        .collect(),
                    v2_clips: state.timeline.tracks[1]
                        .clips
                        .iter()
                        .map(make_clip)
                        .collect(),
                    a1_clips: state.timeline.tracks[2]
                        .clips
                        .iter()
                        .map(make_clip)
                        .collect(),
                    encoder_config: state.encoder_config.clone(),
                    export_filters: state.export_filters.clone(),
                    loudness_normalize: state.loudness_normalize,
                    loudness_target: state.loudness_target,
                };
                state.export = Some(export::spawn_export(snapshot, output_path));
            }
        });
    });

    // Encoder settings row: codec selectors, CRF, Save/Load preset
    ui.horizontal(|ui| {
        ui.label("Video:");
        egui::ComboBox::from_id_salt("vcod")
            .selected_text(state.encoder_config.video_codec.display_name())
            .show_ui(ui, |ui| {
                for codec in [
                    avio::VideoCodec::H264,
                    avio::VideoCodec::H265,
                    avio::VideoCodec::Vp9,
                    avio::VideoCodec::Av1,
                    avio::VideoCodec::ProRes,
                ] {
                    ui.selectable_value(
                        &mut state.encoder_config.video_codec,
                        codec,
                        codec.display_name(),
                    );
                }
            });
        ui.label("Audio:");
        egui::ComboBox::from_id_salt("acod")
            .selected_text(state.encoder_config.audio_codec.display_name())
            .show_ui(ui, |ui| {
                for codec in [
                    avio::AudioCodec::Aac,
                    avio::AudioCodec::Mp3,
                    avio::AudioCodec::Opus,
                    avio::AudioCodec::Flac,
                ] {
                    ui.selectable_value(
                        &mut state.encoder_config.audio_codec,
                        codec,
                        codec.display_name(),
                    );
                }
            });
        ui.label("CRF:");
        ui.add(egui::Slider::new(&mut state.encoder_config.crf, 0..=51));
        if ui.button("Save Preset…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Export Preset", &["json"])
                .set_file_name("preset.json")
                .save_file()
        {
            let pf = PresetFile::from_draft(&state.encoder_config);
            match std::fs::File::create(&path)
                .map_err(|e| e.to_string())
                .and_then(|f| serde_json::to_writer_pretty(f, &pf).map_err(|e| e.to_string()))
            {
                Ok(()) => {}
                Err(e) => log::warn!("save preset failed: {e}"),
            }
        }
        if ui.button("Load Preset…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Export Preset", &["json"])
                .pick_file()
        {
            match std::fs::File::open(&path)
                .map_err(|e| e.to_string())
                .and_then(|f| {
                    serde_json::from_reader::<_, PresetFile>(f).map_err(|e| e.to_string())
                }) {
                Ok(pf) => state.encoder_config = pf.to_draft(),
                Err(e) => log::warn!("load preset failed: {e}"),
            }
        }
    });

    // Filters section
    egui::CollapsingHeader::new("Filters").show(ui, |ui| {
        ui.checkbox(&mut state.export_filters.scale_enabled, "Scale output");
        if state.export_filters.scale_enabled {
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut state.export_filters.output_width)
                        .prefix("W: ")
                        .suffix(" px"),
                );
                ui.add(
                    egui::DragValue::new(&mut state.export_filters.output_height)
                        .prefix("H: ")
                        .suffix(" px"),
                );
            });
        }
        // avio API gap: color balance cannot be applied to Timeline::render().
        // See docs/issue13.md. UI is present for gap documentation purposes.
        ui.checkbox(
            &mut state.export_filters.colorbalance_enabled,
            "Color adjust (UI only — avio gap, not applied during render)",
        );
        if state.export_filters.colorbalance_enabled {
            ui.add(
                egui::Slider::new(&mut state.export_filters.brightness, -1.0..=1.0)
                    .text("Brightness"),
            );
            ui.add(
                egui::Slider::new(&mut state.export_filters.contrast, 0.0..=3.0).text("Contrast"),
            );
            ui.add(
                egui::Slider::new(&mut state.export_filters.saturation, 0.0..=3.0)
                    .text("Saturation"),
            );
        }
    });

    // Loudness measurement
    ui.horizontal(|ui| {
        let can_measure = !state.timeline.tracks[2].clips.is_empty();
        if ui
            .add_enabled(can_measure, egui::Button::new("Measure Loudness"))
            .clicked()
            && let Some(tc) = state.timeline.tracks[2].clips.first()
        {
            let path = state.clips[tc.source_index].path.clone();
            let tx = state.loudness_tx.clone();
            tokio::task::spawn_blocking(move || {
                let result =
                    avio::LoudnessMeter::new(&path)
                        .measure()
                        .ok()
                        .map(|r| state::LoudnessResult {
                            integrated_lufs: r.integrated_lufs,
                            true_peak_dbtp: r.true_peak_dbtp,
                            lra: r.lra,
                        });
                let _ = tx.send(result);
            });
        }
        if let Some(ref r) = state.loudness_result {
            ui.label(format!(
                "I: {:.1} LUFS  TP: {:.1} dBTP  LRA: {:.1} LU",
                r.integrated_lufs, r.true_peak_dbtp, r.lra,
            ));
        }
    });
    // avio API gap: audio_filter() not available on TimelineBuilder (docs/issue13.md).
    ui.horizontal(|ui| {
        ui.checkbox(
            &mut state.loudness_normalize,
            "Normalize to (UI only — avio gap, not applied during render)",
        );
        ui.add(
            egui::DragValue::new(&mut state.loudness_target)
                .range(-40.0..=-5.0)
                .speed(0.5)
                .suffix(" LUFS"),
        );
    });

    // Export status row (shown while running or after completion)
    if let Some(handle) = &state.export {
        let status = handle.status.lock().unwrap().clone();
        match status {
            state::ExportStatus::Running => {
                let pct =
                    f32::from_bits(handle.progress.load(std::sync::atomic::Ordering::Relaxed))
                        / 100.0;
                let bar = egui::ProgressBar::new(pct).animate(pct == 0.0);
                let bar = if pct > 0.0 {
                    bar.text(format!("{:.0}%", pct * 100.0))
                } else {
                    bar.text("Exporting…")
                };
                ui.add(bar);
            }
            state::ExportStatus::Done(path) => {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::GREEN,
                        format!("Exported: {}", path.display()),
                    );
                    if ui.button("Clear").clicked() {
                        clear_export = true;
                    }
                });
            }
            state::ExportStatus::Failed(msg) => {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::RED, format!("Export failed: {msg}"));
                    if ui.button("Dismiss").clicked() {
                        clear_export = true;
                    }
                });
            }
        }
    }
    if clear_export {
        state.export = None;
    }

    // ── Timeline playback controls ────────────────────────────────────────────
    ui.horizontal(|ui| {
        let v1_empty = state.timeline.tracks[0].clips.is_empty();
        let is_playing = state
            .timeline_player_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);
        let is_paused = state.timeline_is_paused;

        if ui
            .add_enabled(!v1_empty && !is_playing, egui::Button::new("▶ Play"))
            .clicked()
        {
            state.stop_source_monitor_player();
            state.stop_timeline_player();
            state.monitor_clip_index = None;

            let clips = &state.clips;
            let make_tcd = |tc: &state::TimelineClip| player::TrackClipData {
                path: clips[tc.source_index].path.clone(),
                start_on_track: tc.start_on_track,
                in_point: tc.in_point,
                out_point: tc.out_point,
                transition: tc.transition,
                transition_duration: tc.transition_duration,
            };
            let v1: Vec<_> = state.timeline.tracks[0]
                .clips
                .iter()
                .map(make_tcd)
                .collect();
            let v2: Vec<_> = state.timeline.tracks[1]
                .clips
                .iter()
                .map(make_tcd)
                .collect();
            let a1: Vec<_> = state.timeline.tracks[2]
                .clips
                .iter()
                .map(make_tcd)
                .collect();

            let start = Duration::from_secs_f64(state.timeline_playhead_secs.max(0.0));
            // Timeline always plays at 1×; reset cpal_rate to 1.0
            state
                .cpal_rate
                .store(1.0f64.to_bits(), std::sync::atomic::Ordering::Relaxed);
            let (thread, handle_rx) = player::spawn_timeline_player(
                v1,
                v2,
                a1,
                Arc::clone(&state.frame_handle),
                ui.ctx().clone(),
                start,
                Arc::clone(&state.cpal_rate),
            );
            state.timeline_player_thread = Some(thread);
            state.timeline_pending_handle_rx = Some(handle_rx);
            state.timeline_is_paused = false;
        }

        if is_playing {
            let pause_label = if is_paused { "⏵ Resume" } else { "⏸ Pause" };
            if ui.button(pause_label).clicked()
                && let Some(h) = &state.timeline_player_handle
            {
                if is_paused {
                    h.play();
                } else {
                    h.pause();
                }
                state.timeline_is_paused = !is_paused;
            }
            if ui.button("⏹ Stop").clicked() {
                state.stop_timeline_player();
            }
        }

        ui.label(format!("{:.2}s", state.timeline_playhead_secs));
    });

    ui.separator();

    const TRACK_HEIGHT: f32 = 40.0;
    const LABEL_WIDTH: f32 = 40.0;

    let pps = state.timeline.pixels_per_second;

    // Dynamic content width: max clip end-time × pps + 200 px padding, min 1200 px.
    let max_end_secs = state
        .timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter_map(|tc| {
            state.clips.get(tc.source_index).map(|c| {
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
    let mut pending_transitions: Vec<(usize, usize, Option<avio::XfadeTransition>, Duration)> =
        Vec::new();
    // (src_track, src_clip, dst_track, new_start_secs)
    let mut pending_moves: Vec<(usize, usize, usize, f32)> = Vec::new();
    let active_drag = state.clip_drag.clone();
    let mut new_drag: Option<state::TimelineClipDrag> = None;
    let mut clear_drag = false;
    let tracks_count = state.timeline.tracks.len();

    egui::ScrollArea::horizontal()
        .id_salt("timeline_scroll")
        .show(ui, |ui| {
            // ── Ruler ──────────────────────────────────────────────────────────
            let (ruler_rect, ruler_resp) = ui.allocate_exact_size(
                egui::vec2(content_width, 24.0),
                egui::Sense::click_and_drag(),
            );
            // Click or drag on ruler to reposition playhead
            if (ruler_resp.clicked() || ruler_resp.dragged())
                && let Some(pos) = ruler_resp.interact_pointer_pos()
            {
                let secs = ((pos.x - ruler_rect.left()) / pps).max(0.0) as f64;
                state.timeline_playhead_secs = secs;
                if let Some(h) = &state.timeline_player_handle {
                    h.seek(Duration::from_secs_f64(secs));
                }
            }
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
            for tc in &state.timeline.tracks[0].clips {
                if let Some(source) = state.clips.get(tc.source_index) {
                    for &scene_ts in &source.scenes {
                        let x =
                            ruler_rect.left() + (tc.start_on_track + scene_ts).as_secs_f32() * pps;
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

            // ── Track lanes ────────────────────────────────────────────────────
            for (track_idx, track) in state.timeline.tracks.iter().enumerate() {
                ui.horizontal(|ui| {
                    // Track label
                    ui.allocate_ui_with_layout(
                        egui::vec2(LABEL_WIDTH, TRACK_HEIGHT),
                        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
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
                    let is_tl_drag_hover = active_drag.is_some()
                        && ui.input(|i| {
                            i.pointer.latest_pos().is_some_and(|ptr| {
                                let y_off = ptr.y - ruler_rect.bottom();
                                ((y_off / TRACK_HEIGHT).floor() as isize) == track_idx as isize
                            })
                        });
                    let bg = if lane_resp.dnd_hover_payload::<usize>().is_some() || is_tl_drag_hover
                    {
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
                    for (clip_i, tc) in track.clips.iter().enumerate() {
                        if let Some(source) = state.clips.get(tc.source_index) {
                            let eff_dur = match (tc.in_point, tc.out_point) {
                                (Some(i), Some(o)) if o > i => o - i,
                                _ => source.info.duration(),
                            };
                            let x = lane_rect.left() + tc.start_on_track.as_secs_f32() * pps;
                            let w = eff_dur.as_secs_f32() * pps;
                            let cr = egui::Rect::from_min_size(
                                egui::pos2(x, lane_rect.top()),
                                egui::vec2(w.max(2.0), TRACK_HEIGHT),
                            );
                            let is_being_dragged = active_drag
                                .as_ref()
                                .is_some_and(|d| d.src_track == track_idx && d.src_clip == clip_i);
                            if cr.max.x >= lane_rect.left() && cr.min.x <= lane_rect.right() {
                                ui.painter().rect_filled(cr, 4.0, clip_color);
                                if is_being_dragged {
                                    ui.painter().rect_filled(
                                        cr,
                                        4.0,
                                        egui::Color32::from_black_alpha(140),
                                    );
                                }

                                // Waveform — A1 track only
                                if track.kind == state::TrackKind::Audio1
                                    && !source.waveform.is_empty()
                                {
                                    let n = source.waveform.len();
                                    let mid_y = cr.center().y;
                                    let half_h = cr.height() * 0.4;
                                    for (i, &amp) in source.waveform.iter().enumerate() {
                                        let x = cr.left() + (i as f32 / n as f32) * cr.width();
                                        if x >= lane_rect.left() && x <= lane_rect.right() {
                                            ui.painter().vline(
                                                x,
                                                (mid_y - amp * half_h)..=(mid_y + amp * half_h),
                                                egui::Stroke::new(
                                                    1.0,
                                                    egui::Color32::from_rgb(100, 200, 100),
                                                ),
                                            );
                                        }
                                    }
                                }

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
                                            + (tc.start_on_track + start).as_secs_f32() * pps;
                                        let sx1 = lane_rect.left()
                                            + (tc.start_on_track + end).as_secs_f32() * pps;
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

                                // Sprite frame tooltip on hover + drag-to-reposition + context menu
                                let clip_id = egui::Id::new(("tl_clip", track_idx, clip_i));
                                let clip_resp =
                                    ui.interact(cr, clip_id, egui::Sense::click_and_drag());

                                if clip_resp.drag_started() {
                                    let ptr_x = clip_resp
                                        .interact_pointer_pos()
                                        .map(|p| p.x)
                                        .unwrap_or(cr.left());
                                    let grab = ((ptr_x - lane_rect.left()) / pps
                                        - tc.start_on_track.as_secs_f32())
                                    .max(0.0);
                                    new_drag = Some(state::TimelineClipDrag {
                                        src_track: track_idx,
                                        src_clip: clip_i,
                                        grab_offset_secs: grab,
                                    });
                                }

                                if clip_resp.drag_stopped()
                                    && let Some(ref drag) = active_drag
                                    && drag.src_track == track_idx
                                    && drag.src_clip == clip_i
                                {
                                    if let Some(ptr) = ui.input(|i| i.pointer.latest_pos()) {
                                        let y_off = ptr.y - ruler_rect.bottom();
                                        let dst_track = ((y_off / TRACK_HEIGHT).floor() as isize)
                                            .clamp(0, tracks_count as isize - 1)
                                            as usize;
                                        let new_start = ((ptr.x - lane_rect.left()) / pps
                                            - drag.grab_offset_secs)
                                            .max(0.0);
                                        pending_moves.push((
                                            drag.src_track,
                                            drag.src_clip,
                                            dst_track,
                                            new_start,
                                        ));
                                    }
                                    clear_drag = true;
                                }

                                if clip_resp.hovered()
                                    && let Some(ss) = &source.sprite_sheet
                                    && let Some(ptr) = ui.input(|i| i.pointer.latest_pos())
                                {
                                    let offset_secs = ((ptr.x - cr.left()) / pps).max(0.0) as f64;
                                    let hover_ts = Duration::from_secs_f64(offset_secs);
                                    let uv = ss.sprite_uv(hover_ts);
                                    egui::Tooltip::always_open(
                                        ui.ctx().clone(),
                                        ui.layer_id(),
                                        egui::Id::new("sprite_tip"),
                                        egui::PopupAnchor::Pointer,
                                    )
                                    .gap(12.0)
                                    .show(|ui| {
                                        ui.add(
                                            egui::Image::new(egui::load::SizedTexture::new(
                                                ss.texture.id(),
                                                egui::vec2(160.0, 90.0),
                                            ))
                                            .uv(uv),
                                        );
                                    });
                                }

                                // Visual indicator — orange stripe when transition set
                                if tc.transition.is_some() {
                                    let indicator = egui::Rect::from_min_size(
                                        cr.min,
                                        egui::vec2(4.0, cr.height()),
                                    )
                                    .intersect(cr);
                                    ui.painter().rect_filled(
                                        indicator,
                                        0.0,
                                        egui::Color32::from_rgb(255, 165, 0),
                                    );
                                }

                                // Context menu on right-click — V1 clips only
                                if track.kind == state::TrackKind::Video1 {
                                    let current_transition = tc.transition;
                                    let mut new_duration_ms =
                                        tc.transition_duration.as_millis() as f64;
                                    clip_resp.context_menu(|ui| {
                                        ui.label("Transition to previous clip:");
                                        for &variant in &[
                                            avio::XfadeTransition::Fade,
                                            avio::XfadeTransition::Dissolve,
                                            avio::XfadeTransition::WipeLeft,
                                            avio::XfadeTransition::WipeRight,
                                            avio::XfadeTransition::SlideDown,
                                        ] {
                                            if ui
                                                .selectable_label(
                                                    current_transition == Some(variant),
                                                    variant.as_str(),
                                                )
                                                .clicked()
                                            {
                                                pending_transitions.push((
                                                    track_idx,
                                                    clip_i,
                                                    Some(variant),
                                                    Duration::from_millis(new_duration_ms as u64),
                                                ));
                                                ui.close();
                                            }
                                        }
                                        ui.separator();
                                        if ui.button("Hard cut (remove)").clicked() {
                                            pending_transitions.push((
                                                track_idx,
                                                clip_i,
                                                None,
                                                Duration::from_millis(500),
                                            ));
                                            ui.close();
                                        }
                                        ui.separator();
                                        ui.label("Duration:");
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut new_duration_ms)
                                                    .range(100.0..=5000.0)
                                                    .speed(10.0)
                                                    .suffix(" ms"),
                                            )
                                            .changed()
                                        {
                                            pending_transitions.push((
                                                track_idx,
                                                clip_i,
                                                current_transition,
                                                Duration::from_millis(new_duration_ms as u64),
                                            ));
                                        }
                                    });
                                }
                            }
                        }
                    }

                    // Drop handling
                    if let Some(clip_idx_arc) = lane_resp.dnd_release_payload::<usize>() {
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
            // ── Ghost clip while dragging ───────────────────────────────────────
            if let Some(ref drag) = active_drag
                && let Some(ptr) = ui.input(|i| i.pointer.latest_pos())
            {
                {
                    let ghost_dur = state
                        .timeline
                        .tracks
                        .get(drag.src_track)
                        .and_then(|t| t.clips.get(drag.src_clip))
                        .and_then(|tc| {
                            state.clips.get(tc.source_index).map(|s| {
                                match (tc.in_point, tc.out_point) {
                                    (Some(i), Some(o)) if o > i => (o - i).as_secs_f32(),
                                    _ => s.info.duration().as_secs_f32(),
                                }
                            })
                        })
                        .unwrap_or(1.0);

                    let tracks_top = ruler_rect.bottom();
                    let y_off = ptr.y - tracks_top;
                    let dst_ti = ((y_off / TRACK_HEIGHT).floor() as isize)
                        .clamp(0, tracks_count as isize - 1)
                        as usize;

                    let ghost_left = ptr.x - drag.grab_offset_secs * pps;
                    let ghost_top = tracks_top + dst_ti as f32 * TRACK_HEIGHT;
                    let ghost_rect = egui::Rect::from_min_size(
                        egui::pos2(ghost_left, ghost_top),
                        egui::vec2((ghost_dur * pps).max(2.0), TRACK_HEIGHT),
                    );
                    ui.painter().rect_filled(
                        ghost_rect,
                        4.0,
                        egui::Color32::from_rgba_premultiplied(100, 160, 220, 100),
                    );
                    ui.painter().rect_stroke(
                        ghost_rect,
                        4.0,
                        egui::Stroke::new(1.5, egui::Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                }
            }

            // ── Playhead ────────────────────────────────────────────────────────
            let playhead_x = ruler_rect.left() + state.timeline_playhead_secs as f32 * pps;
            let tracks_bottom =
                ruler_rect.bottom() + TRACK_HEIGHT * state.timeline.tracks.len() as f32;
            ui.painter().vline(
                playhead_x,
                ruler_rect.top()..=tracks_bottom,
                egui::Stroke::new(2.0, egui::Color32::RED),
            );
        }); // end ScrollArea

    // Apply drag state changes.
    if clear_drag {
        state.clip_drag = None;
    }
    if let Some(nd) = new_drag {
        state.clip_drag = Some(nd);
    }

    // Apply timeline clip moves.
    for (src_track, src_clip, dst_track, new_start_secs) in pending_moves {
        if src_track == dst_track {
            if let Some(clip) = state.timeline.tracks[src_track].clips.get_mut(src_clip) {
                clip.start_on_track = Duration::from_secs_f32(new_start_secs);
            }
        } else if src_clip < state.timeline.tracks[src_track].clips.len() {
            let mut clip = state.timeline.tracks[src_track].clips.remove(src_clip);
            clip.start_on_track = Duration::from_secs_f32(new_start_secs);
            state.timeline.tracks[dst_track].clips.push(clip);
        }
    }

    // Apply drops after the ScrollArea closure to avoid borrow conflicts.
    for (track_idx, clip_idx, start_secs) in pending_clips {
        let (in_pt, out_pt) = state
            .clips
            .get(clip_idx)
            .map(|c| (c.in_point, c.out_point))
            .unwrap_or_default();
        state.timeline.tracks[track_idx]
            .clips
            .push(state::TimelineClip {
                source_index: clip_idx,
                start_on_track: Duration::from_secs_f32(start_secs),
                in_point: in_pt,
                out_point: out_pt,
                transition: None,
                transition_duration: Duration::from_millis(500),
            });
    }
    for (track_idx, clip_i, transition, duration) in pending_transitions {
        if let Some(clip) = state.timeline.tracks[track_idx].clips.get_mut(clip_i) {
            clip.transition = transition;
            clip.transition_duration = duration;
        }
    }
}
