use std::sync::Arc;
use std::time::Duration;

use crate::presets::PresetFile;
use crate::{export, player, state};

pub fn show(state: &mut state::AppState, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();

    // Header: "Timeline" heading + ⚙ settings button + Export button (right-aligned)
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
                let make_clip = |tc: &state::TimelineClip| {
                    let src = &clips[tc.source_index];
                    export::ExportClip {
                        path: src.path.clone(),
                        start_on_track: tc.start_on_track,
                        in_point: tc.in_point,
                        out_point: tc.out_point,
                        transition: tc.transition,
                        transition_duration: tc.transition_duration,
                        source_duration: src.info.duration(),
                        fps: src.info.frame_rate().unwrap_or(30.0),
                    }
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
            ui.toggle_value(&mut state.show_export_settings, "⚙")
                .on_hover_text("Export settings");
        });
    });

    // ── Export Settings modal ─────────────────────────────────────────────────
    egui::Window::new("Export Settings")
        .open(&mut state.show_export_settings)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(&ctx, |ui| {
            // Encoder settings: codec selectors, CRF, Save/Load preset
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
                            avio::AudioCodec::Pcm,
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
            });
            ui.horizontal(|ui| {
                ui.label("CRF:");
                ui.add(egui::Slider::new(&mut state.encoder_config.crf, 0..=51));
            });
            ui.horizontal(|ui| {
                if ui.button("Save Preset…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("Export Preset", &["json"])
                        .set_file_name("preset.json")
                        .save_file()
                {
                    let pf = PresetFile::from_draft(&state.encoder_config);
                    match std::fs::File::create(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|f| {
                            serde_json::to_writer_pretty(f, &pf).map_err(|e| e.to_string())
                        }) {
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

            ui.separator();

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
                    "Color adjust",
                )
                .on_hover_text(
                    "Color balance is not applied during render — avio filter API pending (issue #13)",
                );
                if state.export_filters.colorbalance_enabled {
                    ui.add(
                        egui::Slider::new(&mut state.export_filters.brightness, -1.0..=1.0)
                            .text("Brightness"),
                    );
                    ui.add(
                        egui::Slider::new(&mut state.export_filters.contrast, 0.0..=3.0)
                            .text("Contrast"),
                    );
                    ui.add(
                        egui::Slider::new(&mut state.export_filters.saturation, 0.0..=3.0)
                            .text("Saturation"),
                    );
                }
            });

            ui.separator();

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
                        let result = avio::LoudnessMeter::new(&path)
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
                ui.checkbox(&mut state.loudness_normalize, "Normalize to target LUFS")
                    .on_hover_text(
                        "Render output is not yet normalized — avio audio filter API pending (issue #13)",
                    );
                ui.add(
                    egui::DragValue::new(&mut state.loudness_target)
                        .range(-40.0..=-5.0)
                        .speed(0.5)
                        .suffix(" LUFS"),
                );
            });
        });

    // Export progress window (floating, centered) — shown while running.
    // Done / Failed states remain as an inline status row below.
    if let Some(handle) = &state.export {
        let status = handle
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if matches!(status, state::ExportStatus::Running) {
            let pct = f32::from_bits(handle.progress.load(std::sync::atomic::Ordering::Relaxed));
            egui::Window::new("Exporting…")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(&ctx, |ui| {
                    ui.set_min_width(300.0);
                    let fraction = (pct / 100.0).clamp(0.0, 1.0);
                    let bar = egui::ProgressBar::new(fraction).desired_width(300.0);
                    let bar = if pct > 0.0 {
                        bar.text(format!("{:.0}%", pct))
                    } else {
                        bar.text("Encoding…")
                    };
                    ui.add(bar);
                });
            // Keep the UI updating while the background task runs.
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    // Export completion / failure row
    if let Some(handle) = &state.export {
        let status = handle
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match status {
            state::ExportStatus::Done(path) => {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::GREEN,
                        format!("Exported: {}", path.display()),
                    );
                    if ui.button("✕").clicked() {
                        clear_export = true;
                    }
                });
            }
            state::ExportStatus::Failed(msg) => {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::RED, format!("Export failed: {msg}"));
                    if ui.button("✕").clicked() {
                        clear_export = true;
                    }
                });
            }
            state::ExportStatus::Running => {}
        }
    }
    if clear_export {
        state.export = None;
    }

    // ── Timeline playback controls ────────────────────────────────────────────
    let wants_kb = ui.ctx().wants_keyboard_input();
    let mut do_split =
        !wants_kb && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::X));
    let do_undo = !wants_kb && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Z));
    let do_redo = !wants_kb
        && (ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Y))
            || ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z)
            }));

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
            if ui.button(pause_label).clicked() {
                if is_paused {
                    if state.clips_moved_while_paused {
                        // One or more clips were repositioned while paused.
                        // Send the updated layout to the running runner so it
                        // updates positions in place and seeks to the last known
                        // PTS — no teardown needed.
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
                        match player::build_timeline(v1, v2, a1) {
                            Ok(tl) => {
                                if let Some(h) = &state.timeline_player_handle {
                                    h.update_timeline(tl);
                                }
                            }
                            Err(e) => log::warn!("build_timeline failed: {e}"),
                        }
                        state.clips_moved_while_paused = false;
                    }
                    if let Some(h) = &state.timeline_player_handle {
                        h.play();
                    }
                    state.timeline_is_paused = false;
                } else if let Some(h) = &state.timeline_player_handle {
                    h.pause();
                    state.timeline_is_paused = true;
                }
            }
            if ui.button("⏹ Stop").clicked() {
                state.stop_timeline_player();
                state.timeline_playhead_secs = 0.0;
            }
        }

        if ui
            .add_enabled(!v1_empty, egui::Button::new("✂ Split"))
            .clicked()
        {
            do_split = true;
        }

        ui.label(format!("{:.2}s", state.timeline_playhead_secs));
    });

    ui.separator();

    const TRACK_HEIGHT: f32 = 40.0;
    const LABEL_WIDTH: f32 = 40.0;
    const TRIM_HANDLE_PX: f32 = 6.0;

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
                    (None, Some(o)) => o,
                    (Some(i), None) => c.info.duration().saturating_sub(i),
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
    // (track_idx, clip_idx, is_ripple)
    let mut pending_deletes: Vec<(usize, usize, bool)> = Vec::new();
    // (src_track, src_clip, dst_track, new_start_secs)
    let mut pending_moves: Vec<(usize, usize, usize, f32)> = Vec::new();
    // (track_idx, clip_idx, new_in_point, new_out_point, new_start_on_track)
    #[allow(clippy::type_complexity)]
    let mut pending_trims: Vec<(usize, usize, Option<Duration>, Option<Duration>, Duration)> =
        Vec::new();
    let active_drag = state.clip_drag.clone();
    let active_trim = state.clip_trim.clone();
    let mut new_drag: Option<state::TimelineClipDrag> = None;
    let mut new_trim: Option<state::TimelineClipTrimDrag> = None;
    let mut clear_drag = false;
    let mut clear_trim = false;
    // Set to true when a clip is dropped to a new position while the player is
    // paused. Resume must respawn the player so TimelineRunner gets the updated
    // clip layout; h.play() alone cannot update the runner's internal state.
    let mut moved_while_paused = false;
    let tracks_count = state.timeline.tracks.len();

    egui::ScrollArea::horizontal()
        .id_salt("timeline_scroll")
        .show(ui, |ui| {
            // ── Ruler ──────────────────────────────────────────────────────────
            let (ruler_rect, ruler_resp) = ui.allocate_exact_size(
                egui::vec2(content_width, 24.0),
                egui::Sense::click_and_drag(),
            );
            // Origin shared by ruler ticks, playhead, and clip positions.
            // lane_rect.left() = ruler_rect.left() + LABEL_WIDTH + item_spacing.x,
            // so all time→pixel conversions must use the same offset.
            let timeline_left = ruler_rect.left() + LABEL_WIDTH + ui.spacing().item_spacing.x;
            // Click or drag on ruler to reposition playhead
            if (ruler_resp.clicked() || ruler_resp.dragged())
                && let Some(pos) = ruler_resp.interact_pointer_pos()
            {
                let secs = ((pos.x - timeline_left) / pps).max(0.0) as f64;
                state.timeline_playhead_secs = secs;
                if let Some(h) = &state.timeline_player_handle {
                    h.seek(Duration::from_secs_f64(secs));
                }
            }
            let painter = ui.painter_at(ruler_rect);
            painter.rect_filled(ruler_rect, 0.0, egui::Color32::from_gray(40));

            // Time tick marks every 5 s
            let mut t = 0.0f32;
            while timeline_left + t * pps < ruler_rect.right() {
                let x = timeline_left + t * pps;
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
                        let x = timeline_left + (tc.start_on_track + scene_ts).as_secs_f32() * pps;
                        if x >= timeline_left && x <= ruler_rect.right() {
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
                            let eff_in = tc.in_point.unwrap_or(Duration::ZERO);
                            let eff_dur = match (tc.in_point, tc.out_point) {
                                (Some(i), Some(o)) if o > i => o - i,
                                (None, Some(o)) => o,
                                (Some(i), None) => source.info.duration().saturating_sub(i),
                                _ => source.info.duration(),
                            };
                            let fps = source.info.frame_rate().unwrap_or(30.0) as f32;
                            let one_frame_sec = (1.0 / fps).max(0.001_f32);
                            let orig_x = lane_rect.left() + tc.start_on_track.as_secs_f32() * pps;
                            let orig_w = eff_dur.as_secs_f32() * pps;

                            // Live-preview dimensions during an active trim drag
                            let (live_x, live_w) = if let Some(ref trim) = active_trim {
                                if trim.track_idx == track_idx && trim.clip_idx == clip_i {
                                    if let Some(ptr) = ui.input(|i| i.pointer.latest_pos()) {
                                        match trim.edge {
                                            state::TrimEdge::Right => {
                                                let max_right = orig_x
                                                    + (source.info.duration().as_secs_f32()
                                                        - eff_in.as_secs_f32())
                                                        * pps;
                                                let new_right = ptr
                                                    .x
                                                    .clamp(orig_x + one_frame_sec * pps, max_right);
                                                (orig_x, (new_right - orig_x).max(1.0))
                                            }
                                            state::TrimEdge::Left => {
                                                let right_x = orig_x + orig_w;
                                                let source_left_x = lane_rect.left()
                                                    + (tc.start_on_track.as_secs_f32()
                                                        - eff_in.as_secs_f32())
                                                        * pps;
                                                let min_left = lane_rect.left().max(source_left_x);
                                                let max_left = right_x - one_frame_sec * pps;
                                                let new_left = ptr.x.clamp(min_left, max_left);
                                                (new_left, (right_x - new_left).max(1.0))
                                            }
                                        }
                                    } else {
                                        (orig_x, orig_w)
                                    }
                                } else {
                                    (orig_x, orig_w)
                                }
                            } else {
                                (orig_x, orig_w)
                            };

                            let cr = egui::Rect::from_min_size(
                                egui::pos2(live_x, lane_rect.top()),
                                egui::vec2(live_w.max(2.0), TRACK_HEIGHT),
                            );
                            let is_being_dragged = active_drag
                                .as_ref()
                                .is_some_and(|d| d.src_track == track_idx && d.src_clip == clip_i);
                            if cr.max.x >= lane_rect.left() && cr.min.x <= lane_rect.right() {
                                ui.painter().rect_filled(cr, 4.0, clip_color);

                                // Subtle bright strips at left/right edges to mark trim handles
                                let handle_color = egui::Color32::from_white_alpha(60);
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        cr.min,
                                        egui::vec2(TRIM_HANDLE_PX, cr.height()),
                                    )
                                    .intersect(cr),
                                    0.0,
                                    handle_color,
                                );
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(cr.right() - TRIM_HANDLE_PX, cr.top()),
                                        egui::vec2(TRIM_HANDLE_PX, cr.height()),
                                    )
                                    .intersect(cr),
                                    0.0,
                                    handle_color,
                                );

                                // Filmstrip thumbnails — V1/V2 only
                                if track.kind != state::TrackKind::Audio1
                                    && let Some(ss) = &source.sprite_sheet
                                {
                                    let tile_w = TRACK_HEIGHT * (16.0 / 9.0);
                                    let n_tiles = (cr.width() / tile_w).ceil() as usize + 1;
                                    let in_secs =
                                        tc.in_point.map(|p| p.as_secs_f32()).unwrap_or(0.0);
                                    let first =
                                        ((lane_rect.left() - cr.left()).max(0.0) / tile_w) as usize;
                                    let last = (((lane_rect.right() - cr.left()) / tile_w).ceil()
                                        as usize
                                        + 1)
                                    .min(n_tiles);
                                    let clipped = ui.painter().with_clip_rect(cr);
                                    for i in first..last {
                                        let tile_left = cr.left() + i as f32 * tile_w;
                                        let tile_rect = egui::Rect::from_min_size(
                                            egui::pos2(tile_left, cr.top()),
                                            egui::vec2(tile_w, TRACK_HEIGHT),
                                        );
                                        let src_t = in_secs + (i as f32 + 0.5) * tile_w / pps;
                                        let uv =
                                            ss.sprite_uv(Duration::from_secs_f32(src_t.max(0.0)));
                                        clipped.image(
                                            ss.texture.id(),
                                            tile_rect,
                                            uv,
                                            egui::Color32::WHITE,
                                        );
                                    }
                                    // Darkened tint so text stays readable
                                    ui.painter().rect_filled(
                                        cr,
                                        4.0,
                                        egui::Color32::from_black_alpha(80),
                                    );
                                }

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

                                // Sprite frame tooltip on hover + drag-to-reposition/trim + context menu
                                let clip_id = egui::Id::new(("tl_clip", track_idx, clip_i));
                                let clip_resp =
                                    ui.interact(cr, clip_id, egui::Sense::click_and_drag());

                                // Cursor change and edge-proximity flag for trim handles
                                let near_trim_edge = clip_resp.hovered()
                                    && ui.input(|i| i.pointer.latest_pos()).is_some_and(|ptr| {
                                        ptr.x <= orig_x + TRIM_HANDLE_PX
                                            || ptr.x >= orig_x + orig_w - TRIM_HANDLE_PX
                                    });
                                if near_trim_edge {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                                }

                                if clip_resp.drag_started() {
                                    // Auto-pause so the user can edit clips and
                                    // resume from the exact same playhead frame.
                                    let is_timeline_playing = state
                                        .timeline_player_thread
                                        .as_ref()
                                        .map(|h| !h.is_finished())
                                        .unwrap_or(false);
                                    if is_timeline_playing && !state.timeline_is_paused {
                                        if let Some(h) = &state.timeline_player_handle {
                                            h.pause();
                                        }
                                        state.timeline_is_paused = true;
                                    }

                                    // Use press_origin (the exact click position) rather than
                                    // interact_pointer_pos (current position), which may have
                                    // already drifted outside the 6 px trim handle zone by the
                                    // time egui detects the drag threshold.
                                    let ptr_x = ui
                                        .input(|i| i.pointer.press_origin())
                                        .map(|p| p.x)
                                        .unwrap_or(orig_x);

                                    if ptr_x <= orig_x + TRIM_HANDLE_PX {
                                        new_trim = Some(state::TimelineClipTrimDrag {
                                            track_idx,
                                            clip_idx: clip_i,
                                            edge: state::TrimEdge::Left,
                                        });
                                    } else if ptr_x >= orig_x + orig_w - TRIM_HANDLE_PX {
                                        new_trim = Some(state::TimelineClipTrimDrag {
                                            track_idx,
                                            clip_idx: clip_i,
                                            edge: state::TrimEdge::Right,
                                        });
                                    } else {
                                        let grab = ((ptr_x - lane_rect.left()) / pps
                                            - tc.start_on_track.as_secs_f32())
                                        .max(0.0);
                                        new_drag = Some(state::TimelineClipDrag {
                                            src_track: track_idx,
                                            src_clip: clip_i,
                                            grab_offset_secs: grab,
                                        });
                                    }
                                }

                                if clip_resp.drag_stopped() {
                                    if let Some(ref trim) = active_trim {
                                        if trim.track_idx == track_idx && trim.clip_idx == clip_i {
                                            if let Some(ptr) = ui.input(|i| i.pointer.latest_pos())
                                            {
                                                match trim.edge {
                                                    state::TrimEdge::Right => {
                                                        let max_right = orig_x
                                                            + (source
                                                                .info
                                                                .duration()
                                                                .as_secs_f32()
                                                                - eff_in.as_secs_f32())
                                                                * pps;
                                                        let new_right = ptr.x.clamp(
                                                            orig_x + one_frame_sec * pps,
                                                            max_right,
                                                        );
                                                        let new_out = eff_in
                                                            + Duration::from_secs_f32(
                                                                (new_right - orig_x) / pps,
                                                            );
                                                        pending_trims.push((
                                                            track_idx,
                                                            clip_i,
                                                            tc.in_point,
                                                            Some(new_out),
                                                            tc.start_on_track,
                                                        ));
                                                    }
                                                    state::TrimEdge::Left => {
                                                        let right_x = orig_x + orig_w;
                                                        let source_left_x = lane_rect.left()
                                                            + (tc.start_on_track.as_secs_f32()
                                                                - eff_in.as_secs_f32())
                                                                * pps;
                                                        let min_left =
                                                            lane_rect.left().max(source_left_x);
                                                        let max_left =
                                                            right_x - one_frame_sec * pps;
                                                        let new_left =
                                                            ptr.x.clamp(min_left, max_left);
                                                        let new_start_secs =
                                                            (new_left - lane_rect.left()) / pps;
                                                        let delta = new_start_secs
                                                            - tc.start_on_track.as_secs_f32();
                                                        let new_in = Duration::from_secs_f32(
                                                            (eff_in.as_secs_f32() + delta).max(0.0),
                                                        );
                                                        pending_trims.push((
                                                            track_idx,
                                                            clip_i,
                                                            Some(new_in),
                                                            tc.out_point,
                                                            Duration::from_secs_f32(
                                                                new_start_secs.max(0.0),
                                                            ),
                                                        ));
                                                    }
                                                }
                                            }
                                            clear_trim = true;
                                            if state.timeline_is_paused {
                                                moved_while_paused = true;
                                            }
                                        }
                                    } else if let Some(ref drag) = active_drag
                                        && drag.src_track == track_idx
                                        && drag.src_clip == clip_i
                                    {
                                        if let Some(ptr) = ui.input(|i| i.pointer.latest_pos()) {
                                            let y_off = ptr.y - ruler_rect.bottom();
                                            let dst_track = ((y_off / TRACK_HEIGHT).floor()
                                                as isize)
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
                                            if state.timeline_is_paused {
                                                moved_while_paused = true;
                                            }
                                        }
                                        clear_drag = true;
                                    }
                                }

                                if clip_resp.hovered()
                                    && !near_trim_edge
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

                                // Context menu on right-click — all tracks
                                {
                                    let current_transition = tc.transition;
                                    let mut new_duration_ms =
                                        tc.transition_duration.as_millis() as f64;
                                    clip_resp.context_menu(|ui| {
                                        // Transition options — V1 only
                                        if track.kind == state::TrackKind::Video1 {
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
                                                        Duration::from_millis(
                                                            new_duration_ms as u64,
                                                        ),
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
                                            ui.separator();
                                        }
                                        // Delete options — all tracks
                                        if ui.button("Delete").clicked() {
                                            pending_deletes.push((track_idx, clip_i, false));
                                            ui.close();
                                        }
                                        if ui.button("Ripple Delete").clicked() {
                                            pending_deletes.push((track_idx, clip_i, true));
                                            ui.close();
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
                                    (None, Some(o)) => o.as_secs_f32(),
                                    (Some(i), None) => {
                                        s.info.duration().saturating_sub(i).as_secs_f32()
                                    }
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
            let playhead_x = timeline_left + state.timeline_playhead_secs as f32 * pps;
            let tracks_bottom =
                ruler_rect.bottom() + TRACK_HEIGHT * state.timeline.tracks.len() as f32;
            let playhead_color = egui::Color32::from_rgb(220, 60, 60);
            ui.painter().vline(
                playhead_x,
                ruler_rect.top()..=tracks_bottom,
                egui::Stroke::new(2.0, playhead_color),
            );
            // Triangular drag handle at the top of the ruler
            const HANDLE_W: f32 = 7.0;
            const HANDLE_H: f32 = 11.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(playhead_x, ruler_rect.top() + HANDLE_H),
                    egui::pos2(playhead_x - HANDLE_W, ruler_rect.top()),
                    egui::pos2(playhead_x + HANDLE_W, ruler_rect.top()),
                ],
                playhead_color,
                egui::Stroke::NONE,
            ));
            // Timecode label just to the right of the handle
            let t = state.timeline_playhead_secs;
            let ph_m = (t / 60.0) as u64;
            let ph_s = (t % 60.0) as u64;
            let ph_ms = ((t % 1.0) * 1000.0) as u64;
            ui.painter().text(
                egui::pos2(playhead_x + HANDLE_W + 3.0, ruler_rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                format!("{ph_m:02}:{ph_s:02}.{ph_ms:03}"),
                egui::FontId::monospace(9.0),
                egui::Color32::WHITE,
            );
        }); // end ScrollArea

    // Apply undo/redo — must happen before the snapshot.
    let mut applied_undo_redo = false;
    if do_undo {
        state.apply_undo();
        applied_undo_redo = true;
    }
    if do_redo {
        state.apply_redo();
        applied_undo_redo = true;
    }

    // Snapshot all 3 tracks before applying any pending ops.
    let tracks_before: [Vec<state::TimelineClip>; 3] =
        std::array::from_fn(|i| state.timeline.tracks[i].clips.clone());
    // Flags captured before pending vecs are consumed by for-loops.
    let had_trims = !pending_trims.is_empty();
    let had_moves = !pending_moves.is_empty();
    let had_clips = !pending_clips.is_empty();
    let had_transitions = !pending_transitions.is_empty();
    let had_ripple_delete = pending_deletes.iter().any(|d| d.2);
    let had_deletes = !pending_deletes.is_empty();

    // Apply drag / trim state changes.
    if clear_drag {
        state.clip_drag = None;
    }
    if let Some(nd) = new_drag {
        state.clip_drag = Some(nd);
    }
    if clear_trim {
        state.clip_trim = None;
    }
    if let Some(nt) = new_trim {
        state.clip_trim = Some(nt);
    }
    if moved_while_paused {
        state.clips_moved_while_paused = true;
    }

    // Apply timeline clip trims.
    for (ti, ci, new_in, new_out, new_start) in pending_trims {
        if let Some(clip) = state.timeline.tracks[ti].clips.get_mut(ci) {
            clip.in_point = new_in;
            clip.out_point = new_out;
            clip.start_on_track = new_start;
        }
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

    // Apply deletes in reverse clip order so removals don't shift remaining indices.
    pending_deletes.sort_by(|a, b| b.1.cmp(&a.1));
    for (ti, ci, is_ripple) in pending_deletes {
        if ci < state.timeline.tracks[ti].clips.len() {
            let deleted = state.timeline.tracks[ti].clips.remove(ci);
            if is_ripple {
                let src_dur = state
                    .clips
                    .get(deleted.source_index)
                    .map(|s| s.info.duration())
                    .unwrap_or(Duration::ZERO);
                let eff_dur = match (deleted.in_point, deleted.out_point) {
                    (Some(i), Some(o)) if o > i => o - i,
                    (None, Some(o)) => o,
                    (Some(i), None) => src_dur.saturating_sub(i),
                    _ => src_dur,
                };
                let gap_start = deleted.start_on_track + eff_dur;
                for clip in &mut state.timeline.tracks[ti].clips {
                    if clip.start_on_track >= gap_start {
                        clip.start_on_track = clip.start_on_track.saturating_sub(eff_dur);
                    }
                }
            }
        }
    }

    // Split clips at playhead (C key or "✂ Split" button).
    if do_split {
        let playhead = Duration::from_secs_f64(state.timeline_playhead_secs);
        // Collect: (track_idx, clip_idx, left_out_source, right_start_timeline, orig_out, source_index, transition_duration)
        #[allow(clippy::type_complexity)]
        let mut ops: Vec<(
            usize,
            usize,
            Duration,
            Duration,
            Option<Duration>,
            usize,
            Duration,
        )> = Vec::new();
        for (ti, track) in state.timeline.tracks.iter().enumerate() {
            for (ci, tc) in track.clips.iter().enumerate() {
                if let Some(source) = state.clips.get(tc.source_index) {
                    let eff_in = tc.in_point.unwrap_or(Duration::ZERO);
                    let eff_dur = match (tc.in_point, tc.out_point) {
                        (Some(i), Some(o)) if o > i => o - i,
                        (None, Some(o)) => o,
                        (Some(i), None) => source.info.duration().saturating_sub(i),
                        _ => source.info.duration(),
                    };
                    let clip_end = tc.start_on_track + eff_dur;
                    if playhead > tc.start_on_track && playhead < clip_end {
                        let offset = playhead.saturating_sub(tc.start_on_track);
                        let split_source = eff_in + offset;
                        ops.push((
                            ti,
                            ci,
                            split_source,
                            playhead,
                            tc.out_point,
                            tc.source_index,
                            tc.transition_duration,
                        ));
                    }
                }
            }
        }
        // Apply in reverse clip order so inserts don't shift remaining indices.
        ops.sort_by(|a, b| b.1.cmp(&a.1));
        for (ti, ci, left_out, right_start, right_out, source_index, transition_duration) in ops {
            state.timeline.tracks[ti].clips[ci].out_point = Some(left_out);
            let right = state::TimelineClip {
                source_index,
                start_on_track: right_start,
                in_point: Some(left_out),
                out_point: right_out,
                transition: None,
                transition_duration,
            };
            state.timeline.tracks[ti].clips.insert(ci + 1, right);
        }
        if state.timeline_is_paused {
            state.clips_moved_while_paused = true;
        }
    }

    // Record undo command if tracks changed and this wasn't an undo/redo.
    if !applied_undo_redo {
        let label: &'static str = if do_split {
            "Split Clip"
        } else if had_ripple_delete {
            "Ripple Delete"
        } else if had_deletes {
            "Delete Clip"
        } else if had_moves {
            "Move Clip"
        } else if had_trims {
            "Trim Clip"
        } else if had_transitions {
            "Set Transition"
        } else if had_clips {
            "Add Clip"
        } else {
            ""
        };
        if !label.is_empty() {
            let snapshots: Vec<_> = (0..3_usize)
                .filter(|&i| state.timeline.tracks[i].clips != tracks_before[i])
                .map(|i| {
                    (
                        i,
                        tracks_before[i].clone(),
                        state.timeline.tracks[i].clips.clone(),
                    )
                })
                .collect();
            if !snapshots.is_empty() {
                state.push_edit(state::EditCommand::TrackSnapshot { snapshots, label });
            }
        }
    }
}
