use std::sync::Arc;
use std::time::Duration;

use crate::presets::PresetFile;
use crate::{export, player, state};

fn blend_mode_label(mode: avio::BlendMode) -> &'static str {
    match mode {
        avio::BlendMode::Normal => "Normal",
        avio::BlendMode::Multiply => "Multiply",
        avio::BlendMode::Screen => "Screen",
        avio::BlendMode::Overlay => "Overlay",
        _ => "Custom",
    }
}

/// Returns `true` when track `idx` should contribute clips to the player/exporter.
///
/// Solo takes priority: if any track is soloed, only soloed tracks are active.
/// Otherwise, a track is active unless it is muted.
fn track_is_active(tracks: &[state::Track], idx: usize) -> bool {
    let any_solo = tracks.iter().any(|t| t.soloed);
    if any_solo {
        tracks[idx].soloed
    } else {
        !tracks[idx].muted
    }
}

/// Compute snapped, overlap-free `start_on_track` (seconds) for a dragged clip.
///
/// Snaps the clip's left or right edge to any nearby edge within `snap_px`
/// pixels, then resolves any remaining overlap by pushing the clip to the
/// nearest non-overlapping position.
/// Snap a single trim edge (pixel x) to the nearest clip edge on the same track
/// within `snap_px` pixels. Returns the snapped pixel position.
#[allow(clippy::too_many_arguments)]
fn snap_trim_edge(
    raw_x: f32,
    track_idx: usize,
    clip_i: usize,
    tracks: &[state::Track],
    clips_info: &[state::ImportedClip],
    snap_px: f32,
    lane_left: f32,
    pps: f32,
) -> f32 {
    let mut best_x = raw_x;
    let mut best_dist = snap_px + f32::EPSILON;

    // Snap to timeline origin.
    let d = (raw_x - lane_left).abs();
    if d < best_dist {
        best_dist = d;
        best_x = lane_left;
    }

    if let Some(track) = tracks.get(track_idx) {
        for (ci, clip) in track.clips.iter().enumerate() {
            if ci == clip_i {
                continue;
            }
            let Some(src) = clips_info.get(clip.source_index) else {
                continue;
            };
            let src_dur = match (clip.in_point, clip.out_point) {
                (Some(i), Some(o)) if o > i => (o - i).as_secs_f32(),
                (None, Some(o)) => o.as_secs_f32(),
                (Some(i), None) => src.info.duration().saturating_sub(i).as_secs_f32(),
                _ => src.info.duration().as_secs_f32(),
            };
            let dur = src_dur / clip.speed + freeze_extra_secs(clip.freeze);
            let c_left = lane_left + clip.start_on_track.as_secs_f32() * pps;
            let c_right = c_left + dur * pps;

            for &edge_x in &[c_left, c_right] {
                let d = (raw_x - edge_x).abs();
                if d < best_dist {
                    best_dist = d;
                    best_x = edge_x;
                }
            }
        }
    }

    best_x
}

#[allow(clippy::too_many_arguments)]
fn snap_clip_start(
    raw_start: f32,
    clip_dur: f32,
    dst_track: usize,
    src_track: usize,
    src_clip: usize,
    tracks: &[state::Track],
    clips_info: &[state::ImportedClip],
    snap_px: f32,
    pps: f32,
) -> f32 {
    let snap_secs = snap_px / pps;
    let raw_end = raw_start + clip_dur;

    // Build (start, end) pairs for every clip on dst_track except the dragged clip.
    let others: Vec<(f32, f32)> = tracks
        .get(dst_track)
        .map(|t| {
            t.clips
                .iter()
                .enumerate()
                .filter(|(ci, _)| !(dst_track == src_track && *ci == src_clip))
                .filter_map(|(_, c)| {
                    clips_info.get(c.source_index).map(|s| {
                        let src_dur = match (c.in_point, c.out_point) {
                            (Some(i), Some(o)) if o > i => (o - i).as_secs_f32(),
                            (None, Some(o)) => o.as_secs_f32(),
                            (Some(i), None) => s.info.duration().saturating_sub(i).as_secs_f32(),
                            _ => s.info.duration().as_secs_f32(),
                        };
                        let dur = src_dur / c.speed + freeze_extra_secs(c.freeze);
                        let cs = c.start_on_track.as_secs_f32();
                        (cs, cs + dur)
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Find best snap candidate within snap_secs.
    let mut best = raw_start;
    let mut best_dist = snap_secs + f32::EPSILON;

    // Snap left edge to timeline origin.
    if raw_start < best_dist {
        best_dist = raw_start;
        best = 0.0;
    }
    for &(c_start, c_end) in &others {
        // Our left edge near their right edge.
        let d = (raw_start - c_end).abs();
        if d < best_dist {
            best_dist = d;
            best = c_end;
        }
        // Our right edge near their left edge.
        let d = (raw_end - c_start).abs();
        if d < best_dist {
            best_dist = d;
            best = c_start - clip_dur;
        }
    }

    let mut pos = best.max(0.0);

    // Iteratively push out of any remaining overlap.
    for _ in 0..=others.len() {
        let pos_end = pos + clip_dur;
        let Some(&(c_start, c_end)) = others
            .iter()
            .find(|&&(cs, ce)| pos < ce - 0.001 && pos_end > cs + 0.001)
        else {
            break;
        };
        let left_pos = (c_start - clip_dur).max(0.0);
        let right_pos = c_end;
        pos = if (left_pos - raw_start).abs() <= (right_pos - raw_start).abs() {
            left_pos
        } else {
            right_pos
        };
    }

    pos
}

/// Renders the clip / title inspector (Clip Properties, Title Properties).
/// Shown in the right side panel; empty when nothing is selected.
/// Renders a single-axis position keyframe panel (canvas pixels): a static value
/// DragValue, an "add key at playhead" button (captures the static value at the
/// clip-local playhead time), and the key list with per-key value + easing. Mirrors
/// the opacity keyframe panel; a key's easing controls the ramp to the next key, so it
/// is shown only on keys that have a following segment.
/// Extra timeline seconds a clip occupies because of a freeze-frame hold. #143.
fn freeze_extra_secs(freeze: Option<state::Freeze>) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    freeze.map_or(0.0, |f| f.hold_secs as f32)
}

fn position_keyframe_panel(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    static_val: &mut f32,
    track: &mut state::KeyTrack,
    clip_local: f64,
    unit: &str,
) {
    egui::CollapsingHeader::new(label)
        .id_salt(id)
        .default_open(track.is_active())
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(unit);
                ui.add(
                    egui::DragValue::new(static_val)
                        .speed(1.0)
                        .fixed_decimals(0),
                );
                if ui
                    .button("＋ Key @ playhead")
                    .on_hover_text(format!(
                        "Add a key at {clip_local:.2}s (clip-local) = {:.0}{unit}",
                        *static_val
                    ))
                    .clicked()
                {
                    track.insert(clip_local, f64::from(*static_val), state::KeyEasing::Linear);
                }
                if track.is_active() && ui.button("Clear").clicked() {
                    track.keys.clear();
                }
            });
            if track.is_active() {
                ui.weak("Easing is the ramp to the NEXT key. Re-play to preview edits.");
                let n = track.keys.len();
                let mut to_delete: Option<usize> = None;
                for (i, k) in track.keys.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{:.2}s", k.t_secs));
                        let mut v = k.value;
                        if ui
                            .add(
                                egui::DragValue::new(&mut v)
                                    .speed(1.0)
                                    .suffix(format!(" {unit}"))
                                    .fixed_decimals(0),
                            )
                            .changed()
                        {
                            k.value = v;
                        }
                        if i + 1 < n {
                            egui::ComboBox::from_id_salt((id, "ease", i))
                                .selected_text(k.easing.label())
                                .show_ui(ui, |ui| {
                                    for e in state::KeyEasing::ALL {
                                        ui.selectable_value(&mut k.easing, e, e.label());
                                    }
                                });
                        } else {
                            ui.weak("→ end");
                        }
                        if ui.button("✖").clicked() {
                            to_delete = Some(i);
                        }
                    });
                }
                if let Some(i) = to_delete {
                    track.keys.remove(i);
                }
            } else {
                ui.weak("No keys — value is static.");
            }
        });
}

pub fn show_inspector(state: &mut state::AppState, ui: &mut egui::Ui) {
    ui.heading("Inspector");
    if state.timeline_selected.is_none() && state.selected_title_clip.is_none() {
        ui.weak("Select a clip or title on the timeline to edit its properties.");
        return;
    }
    // Narrower sliders so multi-control rows wrap to fit a side panel of any width
    // (otherwise the wide rows fix the panel to the content width and defeat resize).
    ui.spacing_mut().slider_width = 120.0;
    // ── Clip Properties panel (video tracks only; shown when a clip is selected) ────
    if let Some((ti, ci)) = state.timeline_selected
        && ti < state.timeline.audio_track_start()
    {
        let src_name = state
            .timeline
            .tracks
            .get(ti)
            .and_then(|t| t.clips.get(ci))
            .and_then(|c| state.clips.get(c.source_index))
            .and_then(|s| s.path.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("(clip)")
            .to_owned();
        // Captured before the mutable clip borrow below; used to place opacity
        // keyframes at the current playhead (converted to clip-local time).
        let playhead_secs = state.timeline_playhead_secs;
        // Whether this clip's source carries audio — gates the Volume envelope panel.
        let has_audio = state
            .timeline
            .tracks
            .get(ti)
            .and_then(|t| t.clips.get(ci))
            .and_then(|c| state.clips.get(c.source_index))
            .is_some_and(|s| s.info.primary_audio().is_some());
        if let Some(clip) = state
            .timeline
            .tracks
            .get_mut(ti)
            .and_then(|t| t.clips.get_mut(ci))
        {
            egui::CollapsingHeader::new(format!("Clip Properties — {src_name}"))
                .id_salt("clip_properties")
                .default_open(true)
                .show(ui, |ui| {
                    egui::CollapsingHeader::new("Speed & Compositing")
                        .id_salt("clip_speed_compositing")
                        .default_open(true)
                        .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Speed");
                        let mut speed_pct = clip.speed * 100.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut speed_pct, 10.0..=400.0)
                                    .suffix(" %")
                                    .fixed_decimals(0),
                            )
                            .changed()
                        {
                            clip.speed = (speed_pct / 100.0).clamp(0.1, 4.0);
                        }
                    });
                    // Reverse (export-only) + Freeze frame (hold + extend). #143.
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut clip.reverse, "Reverse").on_hover_text(
                            "Plays backward on export; the preview plays forward (see #178).",
                        );
                    });
                    ui.horizontal(|ui| {
                        let clip_local =
                            (playhead_secs - clip.start_on_track.as_secs_f64()).max(0.0);
                        let mut freeze_on = clip.freeze.is_some();
                        if ui.checkbox(&mut freeze_on, "Freeze frame").changed() {
                            clip.freeze = freeze_on.then_some(state::Freeze {
                                at_secs: clip_local,
                                hold_secs: 1.0,
                            });
                        }
                        if let Some(f) = clip.freeze.as_mut() {
                            ui.label("at");
                            ui.add(
                                egui::DragValue::new(&mut f.at_secs)
                                    .speed(0.1)
                                    .suffix(" s")
                                    .fixed_decimals(2),
                            );
                            ui.label("hold");
                            ui.add(
                                egui::DragValue::new(&mut f.hold_secs)
                                    .speed(0.1)
                                    .suffix(" s")
                                    .fixed_decimals(2),
                            );
                            f.at_secs = f.at_secs.max(0.0);
                            f.hold_secs = f.hold_secs.max(0.1);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Opacity");
                        let mut opacity_pct = clip.opacity * 100.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut opacity_pct, 0.0..=100.0)
                                    .suffix(" %")
                                    .fixed_decimals(0),
                            )
                            .changed()
                        {
                            clip.opacity = (opacity_pct / 100.0).clamp(0.0, 1.0);
                        }
                    });
                    // ── Opacity keyframes ──────────────────────────────────────
                    // Animate opacity over time. Keys are authored at the playhead
                    // (clip-local time) with the current opacity value; both preview
                    // and export animate (avio #1291 / #1292). A key's easing controls
                    // the segment FROM that key TO the next one, so it is shown only on
                    // keys that have a following segment (the last key's easing is unused).
                    egui::CollapsingHeader::new("Opacity Keyframes")
                        .id_salt("clip_opacity_keys")
                        .default_open(clip.animation.opacity.is_active())
                        .show(ui, |ui| {
                            let clip_local =
                                (playhead_secs - clip.start_on_track.as_secs_f64()).max(0.0);
                            ui.horizontal(|ui| {
                                if ui
                                    .button("＋ Key @ playhead")
                                    .on_hover_text(format!(
                                        "Add an opacity key at {clip_local:.2}s (clip-local) \
                                         = {:.0}%",
                                        clip.opacity * 100.0
                                    ))
                                    .clicked()
                                {
                                    clip.animation.opacity.insert(
                                        clip_local,
                                        f64::from(clip.opacity),
                                        state::KeyEasing::Linear,
                                    );
                                }
                                if clip.animation.opacity.is_active()
                                    && ui.button("Clear").clicked()
                                {
                                    clip.animation.opacity.keys.clear();
                                }
                            });
                            if clip.animation.opacity.is_active() {
                                ui.weak(
                                    "Easing is the ramp to the NEXT key. Re-play to preview edits.",
                                );
                                let n = clip.animation.opacity.keys.len();
                                let mut to_delete: Option<usize> = None;
                                for (i, k) in
                                    clip.animation.opacity.keys.iter_mut().enumerate()
                                {
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{:.2}s", k.t_secs));
                                        let mut v_pct = k.value * 100.0;
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut v_pct)
                                                    .range(0.0..=100.0)
                                                    .suffix(" %")
                                                    .fixed_decimals(0),
                                            )
                                            .changed()
                                        {
                                            k.value = (v_pct / 100.0).clamp(0.0, 1.0);
                                        }
                                        // Easing drives the segment to the next key; the
                                        // last key has none, so hide its selector.
                                        if i + 1 < n {
                                            egui::ComboBox::from_id_salt((
                                                "opacity_key_ease",
                                                i,
                                            ))
                                            .selected_text(k.easing.label())
                                            .show_ui(ui, |ui| {
                                                for e in state::KeyEasing::ALL {
                                                    ui.selectable_value(
                                                        &mut k.easing,
                                                        e,
                                                        e.label(),
                                                    );
                                                }
                                            });
                                        } else {
                                            ui.weak("→ end");
                                        }
                                        if ui.button("✖").clicked() {
                                            to_delete = Some(i);
                                        }
                                    });
                                }
                                if let Some(i) = to_delete {
                                    clip.animation.opacity.keys.remove(i);
                                }
                            } else {
                                ui.weak("No keys — opacity is static.");
                            }
                        });
                    // ── Position & Scale keyframes (PiP) — overlay (V2+) clips only ──
                    // Base-layer (V1) position/scale is export-only in the realtime
                    // preview (nothing composites behind the base, and scaling V1 would
                    // change the output size per frame), so these are restricted to
                    // overlay tracks to keep preview == export.
                    if ti >= 1 {
                        let clip_local =
                            (playhead_secs - clip.start_on_track.as_secs_f64()).max(0.0);
                        position_keyframe_panel(
                            ui,
                            "clip_pos_x_keys",
                            "Position X Keyframes",
                            &mut clip.position_x,
                            &mut clip.animation.pos_x,
                            clip_local,
                            "px",
                        );
                        position_keyframe_panel(
                            ui,
                            "clip_pos_y_keys",
                            "Position Y Keyframes",
                            &mut clip.position_y,
                            &mut clip.animation.pos_y,
                            clip_local,
                            "px",
                        );
                        position_keyframe_panel(
                            ui,
                            "clip_scale_keys",
                            "Scale Keyframes",
                            &mut clip.scale_pct,
                            &mut clip.animation.scale,
                            clip_local,
                            "%",
                        );
                    }
                    // ── Volume envelope (dB) — any audio-bearing clip. Overrides the
                    // static per-clip gain when keyed. avio #1316. ──
                    if has_audio {
                        let clip_local =
                            (playhead_secs - clip.start_on_track.as_secs_f64()).max(0.0);
                        position_keyframe_panel(
                            ui,
                            "clip_volume_keys",
                            "Volume Keyframes",
                            &mut clip.gain_db,
                            &mut clip.animation.volume,
                            clip_local,
                            "dB",
                        );
                    }
                    // Blend mode is only meaningful for overlay (V2+) clips.
                    if ti >= 1 {
                        ui.horizontal(|ui| {
                            ui.label("Blend");
                            egui::ComboBox::from_id_salt("clip_blend_mode")
                                .selected_text(blend_mode_label(clip.blend_mode))
                                .show_ui(ui, |ui| {
                                    for mode in [
                                        avio::BlendMode::Normal,
                                        avio::BlendMode::Multiply,
                                        avio::BlendMode::Screen,
                                        avio::BlendMode::Overlay,
                                    ] {
                                        ui.selectable_value(
                                            &mut clip.blend_mode,
                                            mode,
                                            blend_mode_label(mode),
                                        );
                                    }
                                });
                        });
                    }
                        });
                    egui::CollapsingHeader::new("Transform")
                        .id_salt("clip_transform")
                        .default_open(false)
                        .show(ui, |ui| {
                    ui.label("Crop (edge insets)");
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Left");
                        ui.add(
                            egui::Slider::new(&mut clip.transform.crop_left, 0.0..=45.0)
                                .suffix(" %")
                                .fixed_decimals(1),
                        );
                        ui.separator();
                        ui.label("Right");
                        ui.add(
                            egui::Slider::new(&mut clip.transform.crop_right, 0.0..=45.0)
                                .suffix(" %")
                                .fixed_decimals(1),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Top");
                        ui.add(
                            egui::Slider::new(&mut clip.transform.crop_top, 0.0..=45.0)
                                .suffix(" %")
                                .fixed_decimals(1),
                        );
                        ui.separator();
                        ui.label("Bottom");
                        ui.add(
                            egui::Slider::new(&mut clip.transform.crop_bottom, 0.0..=45.0)
                                .suffix(" %")
                                .fixed_decimals(1),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Rotation");
                        ui.add(
                            egui::Slider::new(&mut clip.transform.rotation, -180.0..=180.0)
                                .suffix(" °")
                                .fixed_decimals(1),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut clip.transform.flip_h, "Flip H");
                        ui.checkbox(&mut clip.transform.flip_v, "Flip V");
                        if ui
                            .add_enabled(
                                !clip.transform.is_neutral(),
                                egui::Button::new("Reset"),
                            )
                            .clicked()
                        {
                            clip.transform = state::Transform::default();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Aspect fit");
                        ui.selectable_value(
                            &mut clip.transform.fit_mode,
                            state::FitMode::Fill,
                            "Fill",
                        );
                        ui.selectable_value(
                            &mut clip.transform.fit_mode,
                            state::FitMode::Fit,
                            "Fit",
                        );
                    })
                    .response
                    .on_hover_text(
                        "How this clip fills the project aspect (menu bar → Aspect). \
                         Fill covers and crops; Fit letterboxes. No effect when Aspect is Original.",
                    );
                        });
                    egui::CollapsingHeader::new("Overlay")
                        .id_salt("clip_overlay")
                        .default_open(false)
                        .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let name = clip
                            .overlay
                            .path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("(none)")
                            .to_owned();
                        if ui.button("Choose PNG…").clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("PNG image", &["png"])
                                .pick_file()
                        {
                            clip.overlay.path = Some(path);
                        }
                        if ui
                            .add_enabled(clip.overlay.is_active(), egui::Button::new("Remove"))
                            .clicked()
                        {
                            clip.overlay.path = None;
                        }
                        ui.label(name);
                    });
                    ui.add_enabled_ui(clip.overlay.is_active(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Position");
                            egui::ComboBox::from_id_salt("clip_overlay_pos")
                                .selected_text(clip.overlay.position.label())
                                .show_ui(ui, |ui| {
                                    for pos in state::OverlayPosition::ALL {
                                        ui.selectable_value(
                                            &mut clip.overlay.position,
                                            pos,
                                            pos.label(),
                                        );
                                    }
                                });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Margin");
                            ui.add(
                                egui::DragValue::new(&mut clip.overlay.margin)
                                    .range(0..=500)
                                    .suffix(" px"),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Opacity");
                            let mut opacity_pct = clip.overlay.opacity * 100.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut opacity_pct, 0.0..=100.0)
                                        .suffix(" %")
                                        .fixed_decimals(0),
                                )
                                .changed()
                            {
                                clip.overlay.opacity = (opacity_pct / 100.0).clamp(0.0, 1.0);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Scale");
                            ui.add(
                                egui::Slider::new(&mut clip.overlay.scale, 10.0..=300.0)
                                    .suffix(" %")
                                    .fixed_decimals(0),
                            );
                        })
                        .response
                        .on_hover_text("Overlay size as a percentage of the image's native size.");
                    });
                        });
                    egui::CollapsingHeader::new("Subtitles")
                        .id_salt("clip_subtitles")
                        .default_open(false)
                        .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let name = clip
                            .subtitle
                            .path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .unwrap_or("(none)")
                            .to_owned();
                        if ui.button("Choose SRT/ASS…").clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Subtitles", &["srt", "ass", "ssa"])
                                .pick_file()
                        {
                            let ext = path
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            clip.subtitle.format = if ext == "ass" || ext == "ssa" {
                                state::SubtitleFormat::Ass
                            } else {
                                state::SubtitleFormat::Srt
                            };
                            clip.subtitle.path = Some(path);
                        }
                        if ui
                            .add_enabled(clip.subtitle.is_active(), egui::Button::new("Remove"))
                            .clicked()
                        {
                            clip.subtitle.path = None;
                        }
                        ui.label(name);
                    });
                    if clip.subtitle.is_active() {
                        match clip.subtitle.format {
                            state::SubtitleFormat::Srt => {
                                ui.horizontal(|ui| {
                                    ui.label("Font size");
                                    ui.add(
                                        egui::DragValue::new(&mut clip.subtitle.font_size)
                                            .range(8..=200),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Colour");
                                    ui.color_edit_button_srgb(&mut clip.subtitle.colour);
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Position");
                                    egui::ComboBox::from_id_salt("clip_subtitle_pos")
                                        .selected_text(clip.subtitle.position.label())
                                        .show_ui(ui, |ui| {
                                            for pos in state::SubtitlePosition::ALL {
                                                ui.selectable_value(
                                                    &mut clip.subtitle.position,
                                                    pos,
                                                    pos.label(),
                                                );
                                            }
                                        });
                                });
                            }
                            state::SubtitleFormat::Ass => {
                                ui.weak(
                                    "ASS/SSA uses the file's embedded styles \
                                     (font / colour / position ignored).",
                                );
                            }
                        }
                    }
                    ui.weak(
                        "Burned in per clip — subtitle timing is relative to this clip's start.",
                    );
                        });
                    egui::CollapsingHeader::new("Chroma Key")
                        .id_salt("clip_chroma_key")
                        .default_open(false)
                        .show(ui, |ui| {
                    ui.checkbox(&mut clip.keying.enabled, "Enable chroma key")
                        .on_hover_text(
                            "Most useful on a V2+ overlay clip: the key colour becomes \
                             transparent, revealing the track below.",
                        );
                    ui.add_enabled_ui(clip.keying.is_active(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Mode");
                            ui.selectable_value(
                                &mut clip.keying.mode,
                                state::KeyMode::Chroma,
                                state::KeyMode::Chroma.label(),
                            );
                            ui.selectable_value(
                                &mut clip.keying.mode,
                                state::KeyMode::Color,
                                state::KeyMode::Color.label(),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Key colour");
                            ui.color_edit_button_srgb(&mut clip.keying.color);
                        });
                        ui.horizontal(|ui| {
                            ui.label("Similarity");
                            ui.add(
                                egui::Slider::new(&mut clip.keying.similarity, 0.0..=1.0)
                                    .fixed_decimals(2),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Blend (feather)");
                            ui.add(
                                egui::Slider::new(&mut clip.keying.blend, 0.0..=1.0)
                                    .fixed_decimals(2),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Spill");
                            ui.add(
                                egui::Slider::new(&mut clip.keying.spill, 0.0..=1.0)
                                    .fixed_decimals(2),
                            );
                        })
                        .response
                        .on_hover_text(
                            "Spill suppression is an approximation (global desaturation via \
                             hue), not limited to the key hue.",
                        );
                    });
                        });
                    egui::CollapsingHeader::new("Mask")
                        .id_salt("clip_mask")
                        .default_open(false)
                        .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Shape");
                        egui::ComboBox::from_id_salt("clip_mask_shape")
                            .selected_text(clip.mask.shape.label())
                            .show_ui(ui, |ui| {
                                for s in state::MaskShape::ALL {
                                    ui.selectable_value(&mut clip.mask.shape, s, s.label());
                                }
                            });
                    });
                    if clip.mask.is_active() {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut clip.mask.invert, "Invert");
                            ui.separator();
                            ui.label("Feather");
                            ui.add(
                                egui::DragValue::new(&mut clip.mask.feather)
                                    .range(0..=200)
                                    .suffix(" px"),
                            );
                        });
                        match clip.mask.shape {
                            state::MaskShape::Rectangle => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("X");
                                    ui.add(
                                        egui::Slider::new(&mut clip.mask.rect_x, 0.0..=100.0)
                                            .suffix(" %")
                                            .fixed_decimals(0),
                                    );
                                    ui.separator();
                                    ui.label("Y");
                                    ui.add(
                                        egui::Slider::new(&mut clip.mask.rect_y, 0.0..=100.0)
                                            .suffix(" %")
                                            .fixed_decimals(0),
                                    );
                                });
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("W");
                                    ui.add(
                                        egui::Slider::new(&mut clip.mask.rect_w, 1.0..=100.0)
                                            .suffix(" %")
                                            .fixed_decimals(0),
                                    );
                                    ui.separator();
                                    ui.label("H");
                                    ui.add(
                                        egui::Slider::new(&mut clip.mask.rect_h, 1.0..=100.0)
                                            .suffix(" %")
                                            .fixed_decimals(0),
                                    );
                                });
                            }
                            state::MaskShape::Luma => {
                                ui.horizontal(|ui| {
                                    ui.label("Threshold");
                                    ui.add(
                                        egui::Slider::new(
                                            &mut clip.mask.luma_threshold,
                                            0.0..=1.0,
                                        )
                                        .fixed_decimals(2),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Tolerance");
                                    ui.add(
                                        egui::Slider::new(
                                            &mut clip.mask.luma_tolerance,
                                            0.0..=1.0,
                                        )
                                        .fixed_decimals(2),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Softness");
                                    ui.add(
                                        egui::Slider::new(
                                            &mut clip.mask.luma_softness,
                                            0.0..=1.0,
                                        )
                                        .fixed_decimals(2),
                                    );
                                });
                            }
                            state::MaskShape::Polygon => {
                                ui.horizontal(|ui| {
                                    if ui.button("Reset to quad").clicked() {
                                        clip.mask.polygon = state::Mask::default_quad();
                                    }
                                    if ui
                                        .add_enabled(
                                            !clip.mask.polygon.is_empty(),
                                            egui::Button::new("Clear"),
                                        )
                                        .clicked()
                                    {
                                        clip.mask.polygon.clear();
                                    }
                                    ui.label(format!("{} pts", clip.mask.polygon.len()));
                                });
                                ui.weak(
                                    "On the monitor (paused): drag vertices, double-click to add, \
                                     right-click to remove. Needs 3–16 points.",
                                );
                            }
                            state::MaskShape::None => {}
                        }
                        ui.weak(
                            "Region-composite (garbage matte): keeps the region, rest transparent. \
                             Most useful on a V2 overlay clip.",
                        );
                    }
                        });
                    egui::CollapsingHeader::new("Color Grading")
                        .id_salt("clip_color_grading")
                        .default_open(true)
                        .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Brightness");
                        ui.add(
                            egui::Slider::new(&mut clip.brightness, -1.0..=1.0)
                                .fixed_decimals(2),
                        );
                        ui.separator();
                        ui.label("Contrast");
                        ui.add(
                            egui::Slider::new(&mut clip.contrast, 0.0..=3.0).fixed_decimals(2),
                        );
                        ui.separator();
                        ui.label("Saturation");
                        ui.add(
                            egui::Slider::new(&mut clip.saturation, 0.0..=3.0).fixed_decimals(2),
                        );
                        ui.separator();
                        #[allow(clippy::float_cmp)]
                        let is_neutral =
                            clip.brightness == 0.0 && clip.contrast == 1.0 && clip.saturation == 1.0;
                        if ui
                            .add_enabled(!is_neutral, egui::Button::new("Reset"))
                            .on_hover_text("Reset brightness / contrast / saturation to defaults")
                            .clicked()
                        {
                            clip.brightness = 0.0;
                            clip.contrast = 1.0;
                            clip.saturation = 1.0;
                        }
                    });
                    // White balance — temperature (Kelvin) + tint, via avio WhiteBalance.
                    // Labels precede their sliders (matches the brightness row above).
                    ui.horizontal_wrapped(|ui| {
                        ui.label("White Balance — Temp K");
                        ui.add(egui::Slider::new(&mut clip.wb_temperature, 2000..=12000));
                        ui.separator();
                        ui.label("Tint");
                        ui.add(
                            egui::Slider::new(&mut clip.wb_tint, -0.5..=0.5).fixed_decimals(2),
                        );
                        ui.separator();
                        #[allow(clippy::float_cmp)]
                        let wb_off =
                            clip.wb_temperature == state::WB_NEUTRAL_TEMP && clip.wb_tint == 0.0;
                        if ui
                            .add_enabled(!wb_off, egui::Button::new("Reset"))
                            .on_hover_text("Reset white balance to neutral")
                            .clicked()
                        {
                            clip.wb_temperature = state::WB_NEUTRAL_TEMP;
                            clip.wb_tint = 0.0;
                        }
                    });
                    // Hue rotation + per-channel gamma, via avio Hue / Gamma.
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Hue °");
                        ui.add(
                            egui::Slider::new(&mut clip.hue_degrees, -180.0..=180.0)
                                .fixed_decimals(0),
                        );
                        ui.separator();
                        ui.label("Gamma R");
                        ui.add(egui::Slider::new(&mut clip.gamma_r, 0.1..=3.0).fixed_decimals(2));
                        ui.label("G");
                        ui.add(egui::Slider::new(&mut clip.gamma_g, 0.1..=3.0).fixed_decimals(2));
                        ui.label("B");
                        ui.add(egui::Slider::new(&mut clip.gamma_b, 0.1..=3.0).fixed_decimals(2));
                        ui.separator();
                        #[allow(clippy::float_cmp)]
                        let hsl_off = clip.hue_degrees == 0.0
                            && clip.gamma_r == 1.0
                            && clip.gamma_g == 1.0
                            && clip.gamma_b == 1.0;
                        if ui
                            .add_enabled(!hsl_off, egui::Button::new("Reset"))
                            .on_hover_text("Reset hue and gamma to neutral")
                            .clicked()
                        {
                            clip.hue_degrees = 0.0;
                            clip.gamma_r = 1.0;
                            clip.gamma_g = 1.0;
                            clip.gamma_b = 1.0;
                        }
                    });
                    // Vignette (darkened edges) via avio Vignette. Strength 0 = off;
                    // centre X/Y are percentages of the frame (50 = centre).
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Vignette");
                        ui.add(
                            egui::Slider::new(&mut clip.vignette, 0.0..=100.0).fixed_decimals(0),
                        );
                        ui.separator();
                        let has_vig = clip.vignette > 0.0;
                        ui.add_enabled(
                            has_vig,
                            egui::Slider::new(&mut clip.vignette_x, 0.0..=100.0)
                                .fixed_decimals(0)
                                .text("CX"),
                        );
                        ui.add_enabled(
                            has_vig,
                            egui::Slider::new(&mut clip.vignette_y, 0.0..=100.0)
                                .fixed_decimals(0)
                                .text("CY"),
                        );
                        ui.separator();
                        #[allow(clippy::float_cmp)]
                        let vig_off = clip.vignette == 0.0
                            && clip.vignette_x == 50.0
                            && clip.vignette_y == 50.0;
                        if ui
                            .add_enabled(!vig_off, egui::Button::new("Reset"))
                            .on_hover_text("Reset vignette to neutral")
                            .clicked()
                        {
                            clip.vignette = 0.0;
                            clip.vignette_x = 50.0;
                            clip.vignette_y = 50.0;
                        }
                    });
                        });
                    // Tone curves (Luma + R/G/B) via avio Curves — draggable editor.
                    egui::CollapsingHeader::new("Tone Curves")
                        .id_salt("tone_curves")
                        .show(ui, |ui| {
                            super::curve_editor::tone_curve_editor(ui, &mut clip.curves);
                        });
                    // 3-way colour corrector (lift/gamma/gain) via avio ThreeWayCC.
                    egui::CollapsingHeader::new("Color Wheels")
                        .id_salt("color_wheels")
                        .show(ui, |ui| {
                            super::color_wheels::color_wheels_editor(ui, &mut clip.wheels);
                        });
                    // Stackable video effects via avio FilterSteps (blur, sharpen, …).
                    egui::CollapsingHeader::new("Video Effects")
                        .id_salt("video_effects")
                        .show(ui, |ui| {
                            let fx = &mut clip.video_effects;
                            // Temporal filters (tblend / hqdn3d) need consecutive
                            // frames; the realtime preview pushes a single frame, so
                            // they only show during playback and on export.
                            let temporal_note = "Temporal effect — shows during playback and on export, not on a paused frame.";
                            ui.add(egui::Slider::new(&mut fx.blur, 0.0..=10.0).text("Blur"));
                            ui.add(egui::Slider::new(&mut fx.sharpen, 0.0..=1.5).text("Sharpen"));
                            ui.add(egui::Slider::new(&mut fx.denoise, 0.0..=1.0).text("Denoise"))
                                .on_hover_text(temporal_note);
                            ui.add(egui::Slider::new(&mut fx.grain, 0.0..=100.0).text("Grain"));
                            ui.add(egui::Slider::new(&mut fx.glow, 0.0..=1.0).text("Glow"));
                            ui.add(
                                egui::Slider::new(&mut fx.motion_blur, 0.0..=360.0)
                                    .text("Motion Blur"),
                            )
                            .on_hover_text(temporal_note);
                            ui.add(
                                egui::Slider::new(&mut fx.chromatic_aberration, 0.0..=10.0)
                                    .text("Chromatic Aberration"),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "Denoise / Motion Blur preview during playback & export",
                                )
                                .weak()
                                .small(),
                            );
                            let off = fx.is_neutral();
                            if ui
                                .add_enabled(!off, egui::Button::new("Reset"))
                                .on_hover_text("Reset all video effects")
                                .clicked()
                            {
                                *fx = state::VideoEffects::default();
                            }
                        });
                    // 3D LUT (.cube) — export-only via avio Clip effect chain.
                    ui.horizontal(|ui| {
                        ui.label("LUT (.cube)");
                        if ui.button("Load…").clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Cube LUT", &["cube"])
                                .pick_file()
                        {
                            clip.lut_path = Some(path);
                        }
                        if clip.lut_path.is_some() && ui.button("Clear").clicked() {
                            clip.lut_path = None;
                        }
                        if let Some(p) = &clip.lut_path {
                            let name = p
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.weak(name);
                        }
                    });
                    egui::CollapsingHeader::new("Notes")
                        .id_salt("clip_properties_notes")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.weak(
                                "Grade, LUT, colour wheels, tone curves, and video effects apply in both the preview and the export via the avio compositor. Temporal video effects (Motion Blur and the temporal part of Denoise) show during playback and on export, but not on a paused frame (docs/issue66.md). Speed changes audio pitch proportionally in the preview — no pitch correction (docs/issue42.md).",
                            );
                        });
                });
        }
    }

    // ── Title Editor ──────────────────────────────────────────────────────────
    if let Some(tci) = state.selected_title_clip
        && let Some(tc) = state.timeline.title_clips.get_mut(tci)
    {
        egui::CollapsingHeader::new("Title Properties")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Text");
                    ui.text_edit_multiline(&mut tc.text);
                });
                ui.horizontal(|ui| {
                    ui.label("Font size");
                    let mut fs = tc.font_size as f32;
                    if ui
                        .add(
                            egui::Slider::new(&mut fs, 12.0..=120.0)
                                .suffix(" pt")
                                .fixed_decimals(0),
                        )
                        .changed()
                    {
                        tc.font_size = fs as u32;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Color");
                    let mut color = egui::Color32::from_rgba_unmultiplied(
                        tc.color[0],
                        tc.color[1],
                        tc.color[2],
                        tc.color[3],
                    );
                    if egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut color,
                        egui::color_picker::Alpha::OnlyBlend,
                    )
                    .changed()
                    {
                        tc.color = color.to_array();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("H-Align");
                    ui.selectable_value(&mut tc.h_align, state::HAlign::Left, "Left");
                    ui.selectable_value(&mut tc.h_align, state::HAlign::Centre, "Centre");
                    ui.selectable_value(&mut tc.h_align, state::HAlign::Right, "Right");
                });
                ui.horizontal(|ui| {
                    ui.label("V-Align");
                    ui.selectable_value(&mut tc.v_align, state::VAlign::Top, "Top");
                    ui.selectable_value(&mut tc.v_align, state::VAlign::Middle, "Middle");
                    ui.selectable_value(&mut tc.v_align, state::VAlign::Bottom, "Bottom");
                });
                ui.horizontal(|ui| {
                    ui.label("Start");
                    let mut start_secs = tc.start_on_track.as_secs_f32();
                    if ui
                        .add(
                            egui::DragValue::new(&mut start_secs)
                                .suffix(" s")
                                .speed(0.1),
                        )
                        .changed()
                    {
                        tc.start_on_track = Duration::from_secs_f32(start_secs.max(0.0));
                    }
                    ui.label("Duration");
                    let mut dur_secs = tc.duration.as_secs_f32();
                    if ui
                        .add(egui::DragValue::new(&mut dur_secs).suffix(" s").speed(0.1))
                        .changed()
                    {
                        tc.duration = Duration::from_secs_f32(dur_secs.max(0.1));
                    }
                });
                // avio gap: title clips are UI-only; TimelineBuilder has no drawtext API (docs/issue47.md).
                ui.weak(
                    "Title clips render in the UI only and are not exported \
                         (avio gap — docs/issue47.md).",
                );
            });
    }
}

pub fn show(state: &mut state::AppState, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();

    // Header: "Timeline" heading + ⚙ settings button + queue buttons (right-aligned)
    ui.horizontal(|ui| {
        ui.heading("Timeline");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let v1_empty = state.timeline.tracks[0].clips.is_empty();
            let any_running = state.export_queue.iter().any(|j| {
                matches!(
                    *j.status.lock().unwrap_or_else(|e| e.into_inner()),
                    state::QueueJobStatus::Running
                )
            });
            let has_pending = state.export_queue.iter().any(|j| {
                matches!(
                    *j.status.lock().unwrap_or_else(|e| e.into_inner()),
                    state::QueueJobStatus::Pending
                )
            });
            if ui
                .add_enabled(
                    !state.export_queue.is_empty() && !any_running && has_pending,
                    egui::Button::new("Render All"),
                )
                .clicked()
            {
                state.queue_rendering = true;
            }
            if ui
                .add_enabled(!v1_empty, egui::Button::new("Add to Queue"))
                .clicked()
                && let Some(output_path) = rfd::FileDialog::new()
                    .add_filter("MP4", &["mp4"])
                    .set_file_name("export.mp4")
                    .save_file()
            {
                let clips = &state.clips;
                let use_proxies = state.export_use_proxies;
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
                        has_audio: src.info.primary_audio().is_some(),
                        gain_db: tc.gain_db,
                        fade_in: tc.fade_in,
                        fade_out: tc.fade_out,
                        brightness: tc.brightness,
                        contrast: tc.contrast,
                        saturation: tc.saturation,
                        speed: tc.speed,
                        reverse: tc.reverse,
                        freeze: tc.freeze,
                        opacity: tc.opacity,
                        blend_mode: tc.blend_mode,
                        position_x: tc.position_x,
                        position_y: tc.position_y,
                        scale_pct: tc.scale_pct,
                        proxy_path: if use_proxies {
                            src.proxy_path.clone()
                        } else {
                            None
                        },
                        lut_path: tc.lut_path.clone(),
                        wb_temperature: tc.wb_temperature,
                        wb_tint: tc.wb_tint,
                        hue_degrees: tc.hue_degrees,
                        gamma_r: tc.gamma_r,
                        gamma_g: tc.gamma_g,
                        gamma_b: tc.gamma_b,
                        vignette: tc.vignette,
                        vignette_x: tc.vignette_x,
                        vignette_y: tc.vignette_y,
                        width: src.info.primary_video().map(|v| v.width()).unwrap_or(0),
                        height: src.info.primary_video().map(|v| v.height()).unwrap_or(0),
                        curves: tc.curves.clone(),
                        wheels: tc.wheels,
                        video_effects: tc.video_effects,
                        transform: tc.transform,
                        overlay: tc.overlay.clone(),
                        subtitle: tc.subtitle.clone(),
                        keying: tc.keying,
                        mask: tc.mask.clone(),
                        animation: tc.animation.clone(),
                    }
                };
                let tracks = &state.timeline.tracks;
                let audio_start = state.timeline.audio_track_start();
                let snapshot = export::ExportSnapshot {
                    video_clips: (0..audio_start)
                        .map(|ti| {
                            if track_is_active(tracks, ti) {
                                tracks[ti].clips.iter().map(make_clip).collect()
                            } else {
                                vec![]
                            }
                        })
                        .collect(),
                    a1_clips: if audio_start < tracks.len() && track_is_active(tracks, audio_start)
                    {
                        tracks[audio_start].clips.iter().map(make_clip).collect()
                    } else {
                        vec![]
                    },
                    encoder_config: state.encoder_config.clone(),
                    export_filters: state.export_filters.clone(),
                    loudness_normalize: state.loudness_normalize,
                    loudness_target: state.loudness_target,
                    title_clips: state.timeline.title_clips.clone(),
                    export_in: if state.export_range_enabled {
                        state.export_in
                    } else {
                        None
                    },
                    export_out: if state.export_range_enabled {
                        state.export_out
                    } else {
                        None
                    },
                    canvas: state.project_aspect.dims(),
                };
                state
                    .export_queue
                    .push(export::QueueJob::new(snapshot, output_path));
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

            // Export Range section
            ui.horizontal(|ui| {
                ui.checkbox(&mut state.export_range_enabled, "Export Range only");
                if state.export_range_enabled {
                    match (state.export_in, state.export_out) {
                        (Some(ei), Some(eo)) if ei < eo => {
                            let d = (eo - ei).as_secs_f64();
                            ui.label(format!(
                                "{:02}:{:02}.{:03}",
                                (d / 60.0) as u64,
                                (d % 60.0) as u64,
                                ((d % 1.0) * 1000.0) as u64
                            ));
                        }
                        _ => {
                            ui.weak("Set I/O markers on ruler");
                        }
                    }
                }
            });
            if state.export_range_enabled {
                let fmt_tc = |d: Option<std::time::Duration>| {
                    d.map(|d| {
                        let s = d.as_secs_f64();
                        format!(
                            "{:02}:{:02}.{:03}",
                            (s / 60.0) as u64,
                            (s % 60.0) as u64,
                            ((s % 1.0) * 1000.0) as u64
                        )
                    })
                    .unwrap_or_else(|| "--:--.---".to_string())
                };
                ui.horizontal(|ui| {
                    ui.label(format!("IN: {}", fmt_tc(state.export_in)));
                    if ui.small_button("Set").clicked() {
                        state.export_in = Some(std::time::Duration::from_secs_f64(
                            state.timeline_playhead_secs,
                        ));
                    }
                    ui.separator();
                    ui.label(format!("OUT: {}", fmt_tc(state.export_out)));
                    if ui.small_button("Set").clicked() {
                        state.export_out = Some(std::time::Duration::from_secs_f64(
                            state.timeline_playhead_secs,
                        ));
                    }
                });
            }

            ui.checkbox(
                &mut state.export_use_proxies,
                "Use proxies for export (faster, lower quality)",
            )
            .on_hover_text(
                "Decode from each clip's proxy (when generated) and scale up to the \
                 original resolution. Uses avio's Clip::proxy API.",
            );

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
                let audio_start = state.timeline.audio_track_start();
                let can_measure = state.timeline.tracks.get(audio_start).is_some_and(|t| !t.clips.is_empty());
                if ui
                    .add_enabled(can_measure, egui::Button::new("Measure Loudness"))
                    .clicked()
                    && let Some(tc) = state.timeline.tracks.get(audio_start).and_then(|t| t.clips.first())
                {
                    let path = state.clips[tc.source_index].path.clone();
                    let tx = state.loudness_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = ff_filter::LoudnessMeter::new(&path)
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

            // ── Queue list ─────────────────────────────────────────────────────
            ui.separator();
            ui.label("Queue");
            let mut remove_idx: Option<usize> = None;
            for (i, job) in state.export_queue.iter().enumerate() {
                let status = job
                    .status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let filename = job
                    .output_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&filename)
                            .text_style(egui::TextStyle::Monospace),
                    );
                    match &status {
                        state::QueueJobStatus::Pending => {
                            ui.weak("Pending");
                            if ui.small_button("Remove").clicked() {
                                remove_idx = Some(i);
                            }
                        }
                        state::QueueJobStatus::Running => {
                            let pct = f32::from_bits(
                                job.progress
                                    .load(std::sync::atomic::Ordering::Relaxed),
                            );
                            let fraction = (pct / 100.0).clamp(0.0, 1.0);
                            let bar_text = if pct > 0.0 {
                                format!("{:.0}%", pct)
                            } else {
                                "Encoding…".to_string()
                            };
                            ui.add(
                                egui::ProgressBar::new(fraction)
                                    .desired_width(120.0)
                                    .text(bar_text),
                            );
                            if ui.small_button("Cancel").clicked() {
                                job.cancel
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        state::QueueJobStatus::Done(_) => {
                            ui.colored_label(egui::Color32::GREEN, "Done");
                            if ui.small_button("✕").clicked() {
                                remove_idx = Some(i);
                            }
                        }
                        state::QueueJobStatus::Failed(msg) => {
                            ui.colored_label(egui::Color32::RED, "Failed")
                                .on_hover_text(msg.as_str());
                            if ui.small_button("✕").clicked() {
                                remove_idx = Some(i);
                            }
                        }
                        state::QueueJobStatus::Cancelled => {
                            ui.weak("Cancelled");
                            if ui.small_button("✕").clicked() {
                                remove_idx = Some(i);
                            }
                        }
                    }
                });
            }
            if let Some(i) = remove_idx {
                state.export_queue.remove(i);
            }
        });

    // ── Export queue advancement ──────────────────────────────────────────────
    // Each frame: when no job is running, start the next Pending one.
    if state.queue_rendering {
        let any_running = state.export_queue.iter().any(|j| {
            matches!(
                *j.status.lock().unwrap_or_else(|e| e.into_inner()),
                state::QueueJobStatus::Running
            )
        });
        if !any_running {
            if let Some(job) = state.export_queue.iter_mut().find(|j| {
                matches!(
                    *j.status.lock().unwrap_or_else(|e| e.into_inner()),
                    state::QueueJobStatus::Pending
                )
            }) {
                export::spawn_queue_job(job);
            } else {
                state.queue_rendering = false;
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
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
    // egui-winit converts Ctrl+C → Event::Copy and Ctrl+V → Event::Paste on Windows,
    // so consume_key(Ctrl, C/V) never fires; check the clipboard events first.
    let do_copy = !wants_kb
        && ui.input_mut(|i| {
            if let Some(pos) = i.events.iter().position(|e| matches!(e, egui::Event::Copy)) {
                i.events.remove(pos);
                true
            } else {
                i.consume_key(egui::Modifiers::CTRL, egui::Key::C)
            }
        });
    let do_paste = !wants_kb
        && ui.input_mut(|i| {
            if let Some(pos) = i
                .events
                .iter()
                .position(|e| matches!(e, egui::Event::Paste(_)))
            {
                i.events.remove(pos);
                true
            } else {
                i.consume_key(egui::Modifiers::CTRL, egui::Key::V)
            }
        });
    let do_duplicate =
        !wants_kb && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::D));
    let do_loop_in =
        !wants_kb && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::I));
    let do_loop_out =
        !wants_kb && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::O));

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
                gain_db: tc.gain_db,
                fade_in: tc.fade_in,
                fade_out: tc.fade_out,
                brightness: tc.brightness,
                contrast: tc.contrast,
                saturation: tc.saturation,
                wb_temperature: tc.wb_temperature,
                wb_tint: tc.wb_tint,
                hue_degrees: tc.hue_degrees,
                gamma_r: tc.gamma_r,
                gamma_g: tc.gamma_g,
                gamma_b: tc.gamma_b,
                lut_path: tc.lut_path.clone(),
                speed: tc.speed,
                freeze: tc.freeze,
                opacity: tc.opacity,
                blend_mode: tc.blend_mode,
                position_x: tc.position_x,
                position_y: tc.position_y,
                scale_pct: tc.scale_pct,
                vignette: tc.vignette,
                vignette_x: tc.vignette_x,
                vignette_y: tc.vignette_y,
                width: clips[tc.source_index]
                    .info
                    .primary_video()
                    .map(|v| v.width())
                    .unwrap_or(0),
                height: clips[tc.source_index]
                    .info
                    .primary_video()
                    .map(|v| v.height())
                    .unwrap_or(0),
                curves: tc.curves.clone(),
                wheels: tc.wheels,
                video_effects: tc.video_effects,
                transform: tc.transform,
                overlay: tc.overlay.clone(),
                subtitle: tc.subtitle.clone(),
                keying: tc.keying,
                mask: tc.mask.clone(),
                animation: tc.animation.clone(),
            };
            let tracks = &state.timeline.tracks;
            let audio_start = state.timeline.audio_track_start();
            let video_tracks: Vec<Vec<_>> = (0..audio_start)
                .map(|ti| {
                    if track_is_active(tracks, ti) {
                        tracks[ti].clips.iter().map(make_tcd).collect()
                    } else {
                        vec![]
                    }
                })
                .collect();
            let a1: Vec<_> = if audio_start < tracks.len() && track_is_active(tracks, audio_start) {
                tracks[audio_start].clips.iter().map(make_tcd).collect()
            } else {
                vec![]
            };

            let start = Duration::from_secs_f64(state.timeline_playhead_secs.max(0.0));
            // Timeline always plays at 1×; reset cpal_rate to 1.0
            state
                .cpal_rate
                .store(1.0f64.to_bits(), std::sync::atomic::Ordering::Relaxed);
            let (thread, handle_rx) = player::spawn_timeline_player(
                video_tracks,
                a1,
                Arc::clone(&state.frame_handle),
                ui.ctx().clone(),
                start,
                Arc::clone(&state.cpal_rate),
                state.project_aspect.dims(),
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
                        // Clips were added, removed, or repositioned while paused.
                        // update_layout_in_place fails when the clip count changed
                        // (split, duplicate, delete), so always fully restart the
                        // runner from the current playhead position.
                        let resume_pos =
                            Duration::from_secs_f64(state.timeline_playhead_secs.max(0.0));
                        state.stop_timeline_player();
                        let clips = &state.clips;
                        let make_tcd = |tc: &state::TimelineClip| player::TrackClipData {
                            path: clips[tc.source_index].path.clone(),
                            start_on_track: tc.start_on_track,
                            in_point: tc.in_point,
                            out_point: tc.out_point,
                            transition: tc.transition,
                            transition_duration: tc.transition_duration,
                            gain_db: tc.gain_db,
                            fade_in: tc.fade_in,
                            fade_out: tc.fade_out,
                            brightness: tc.brightness,
                            contrast: tc.contrast,
                            saturation: tc.saturation,
                            wb_temperature: tc.wb_temperature,
                            wb_tint: tc.wb_tint,
                            hue_degrees: tc.hue_degrees,
                            gamma_r: tc.gamma_r,
                            gamma_g: tc.gamma_g,
                            gamma_b: tc.gamma_b,
                            lut_path: tc.lut_path.clone(),
                            speed: tc.speed,
                            freeze: tc.freeze,
                            opacity: tc.opacity,
                            blend_mode: tc.blend_mode,
                            position_x: tc.position_x,
                            position_y: tc.position_y,
                            scale_pct: tc.scale_pct,
                            vignette: tc.vignette,
                            vignette_x: tc.vignette_x,
                            vignette_y: tc.vignette_y,
                            width: clips[tc.source_index]
                                .info
                                .primary_video()
                                .map(|v| v.width())
                                .unwrap_or(0),
                            height: clips[tc.source_index]
                                .info
                                .primary_video()
                                .map(|v| v.height())
                                .unwrap_or(0),
                            curves: tc.curves.clone(),
                            wheels: tc.wheels,
                            video_effects: tc.video_effects,
                            transform: tc.transform,
                            overlay: tc.overlay.clone(),
                            subtitle: tc.subtitle.clone(),
                            keying: tc.keying,
                            mask: tc.mask.clone(),
                            animation: tc.animation.clone(),
                        };
                        let tracks = &state.timeline.tracks;
                        let audio_start = state.timeline.audio_track_start();
                        let video_tracks: Vec<Vec<_>> = (0..audio_start)
                            .map(|ti| {
                                if track_is_active(tracks, ti) {
                                    tracks[ti].clips.iter().map(make_tcd).collect()
                                } else {
                                    vec![]
                                }
                            })
                            .collect();
                        let a1: Vec<_> =
                            if audio_start < tracks.len() && track_is_active(tracks, audio_start) {
                                tracks[audio_start].clips.iter().map(make_tcd).collect()
                            } else {
                                vec![]
                            };
                        state
                            .cpal_rate
                            .store(1.0f64.to_bits(), std::sync::atomic::Ordering::Relaxed);
                        let (thread, handle_rx) = player::spawn_timeline_player(
                            video_tracks,
                            a1,
                            Arc::clone(&state.frame_handle),
                            ui.ctx().clone(),
                            resume_pos,
                            Arc::clone(&state.cpal_rate),
                            state.project_aspect.dims(),
                        );
                        state.timeline_player_thread = Some(thread);
                        state.timeline_pending_handle_rx = Some(handle_rx);
                        state.clips_moved_while_paused = false;
                        state.timeline_is_paused = false;
                    } else {
                        if let Some(h) = &state.timeline_player_handle {
                            h.play();
                        }
                        state.timeline_is_paused = false;
                    }
                } else if let Some(h) = &state.timeline_player_handle {
                    h.pause();
                    state.timeline_is_paused = true;
                }
            }
            if ui.button("⏹ Stop").clicked() {
                state.stop_timeline_player();
                state.timeline_playhead_secs = 0.0;
                state.timeline_loop_enabled = false;
            }
            ui.separator();
            if ui
                .selectable_label(state.timeline_loop_enabled, "⟲")
                .on_hover_text("Loop between I/O markers (I = in, O = out)")
                .clicked()
            {
                state.timeline_loop_enabled = !state.timeline_loop_enabled;
            }
        }

        if ui
            .add_enabled(!v1_empty, egui::Button::new("✂ Split"))
            .clicked()
        {
            do_split = true;
        }

        ui.label(format!("{:.2}s", state.timeline_playhead_secs));

        ui.separator();
        if ui.button("+ Video Track").clicked() {
            state.timeline.add_video_track();
        }
    });

    ui.separator();

    const TRACK_HEIGHT: f32 = 40.0;
    const LABEL_WIDTH: f32 = 80.0;
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
                let src_dur = match (tc.in_point, tc.out_point) {
                    (Some(i), Some(o)) if o > i => o - i,
                    (None, Some(o)) => o,
                    (Some(i), None) => c.info.duration().saturating_sub(i),
                    _ => c.info.duration(),
                };
                tc.start_on_track.as_secs_f32()
                    + src_dur.as_secs_f32() / tc.speed
                    + freeze_extra_secs(tc.freeze)
            })
        })
        .fold(0.0f32, f32::max);
    // Also account for title clip extents.
    let max_end_secs = state
        .timeline
        .title_clips
        .iter()
        .map(|tc| tc.start_on_track.as_secs_f32() + tc.duration.as_secs_f32())
        .fold(max_end_secs, f32::max);
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
    // (track_idx, clip) — clips to insert at end of their track (paste / duplicate)
    let mut pending_inserts: Vec<(usize, state::TimelineClip)> = Vec::new();
    // (track_idx, clip_idx, new_gain_db) — gain line drags on A1 clips
    let mut pending_gain: Vec<(usize, usize, f32)> = Vec::new();
    // (track_idx, clip_idx, new_fade_in, new_fade_out) — fade handle drags on A1 clips
    let mut pending_fades: Vec<(usize, usize, Option<Duration>, Option<Duration>)> = Vec::new();
    // (track_idx, clip_idx, new_speed) — speed changes from context menu
    let mut pending_speeds: Vec<(usize, usize, f32)> = Vec::new();
    // track_idx — M or S button clicked this frame
    let mut pending_mute_toggle: Option<usize> = None;
    let mut pending_solo_toggle: Option<usize> = None;
    // T1 title clip actions
    let mut new_title_selection: Option<usize> = None;
    let mut delete_title_clip: Option<usize> = None;
    // Text preset drops from the browser: (preset_idx, start_secs_on_track)
    let mut pending_title_drops: Vec<(usize, f32)> = Vec::new();
    // Title clip drag-to-reposition: (clip_idx, new_start_secs)
    let mut pending_title_moves: Vec<(usize, f32)> = Vec::new();
    // Title clip edge-trim: (clip_idx, new_start, new_duration)
    let mut pending_title_trims: Vec<(usize, Duration, Duration)> = Vec::new();
    let active_title_drag = state.title_clip_drag.clone();
    let active_title_trim = state.title_clip_trim.clone();
    let mut new_title_drag: Option<state::TitleClipDrag> = None;
    let mut new_title_trim: Option<state::TitleClipTrimDrag> = None;
    let mut clear_title_drag = false;
    let mut clear_title_trim = false;
    // Set on clip left-click; applied after the ScrollArea.
    let mut new_selection: Option<(usize, usize)> = None;
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

            // I/O markers: shaded band + marker lines (shared by loop and export range)
            if let (Some(li), Some(lo)) = (state.export_in, state.export_out)
                && li < lo
            {
                let x1 = (timeline_left + li.as_secs_f32() * pps).max(timeline_left);
                let x2 = (timeline_left + lo.as_secs_f32() * pps).min(ruler_rect.right());
                if x1 < x2 {
                    painter.rect_filled(
                        egui::Rect::from_x_y_ranges(x1..=x2, ruler_rect.y_range()),
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(100, 200, 255, 40),
                    );
                }
            }
            if let Some(li) = state.export_in {
                let x = timeline_left + li.as_secs_f32() * pps;
                if x >= timeline_left && x <= ruler_rect.right() {
                    painter.vline(
                        x,
                        ruler_rect.y_range(),
                        egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 200, 255)),
                    );
                    painter.text(
                        egui::pos2(x + 2.0, ruler_rect.top() + 2.0),
                        egui::Align2::LEFT_TOP,
                        "I",
                        egui::FontId::monospace(9.0),
                        egui::Color32::from_rgb(100, 200, 255),
                    );
                }
            }
            if let Some(lo) = state.export_out {
                let x = timeline_left + lo.as_secs_f32() * pps;
                if x >= timeline_left && x <= ruler_rect.right() {
                    painter.vline(
                        x,
                        ruler_rect.y_range(),
                        egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 200, 255)),
                    );
                    painter.text(
                        egui::pos2(x + 2.0, ruler_rect.top() + 2.0),
                        egui::Align2::LEFT_TOP,
                        "O",
                        egui::FontId::monospace(9.0),
                        egui::Color32::from_rgb(100, 200, 255),
                    );
                }
            }

            // ── Title track (T1) — rendered above all video tracks ─────────────
            let mut t1_lane_rect = egui::Rect::NOTHING;
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(LABEL_WIDTH, TRACK_HEIGHT),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.label(
                            egui::RichText::new("T1")
                                .strong()
                                .color(egui::Color32::from_rgb(200, 150, 50)),
                        )
                        .on_hover_text("Title track");
                    },
                );

                let is_text_dnd_hover = ui.input(|i| i.pointer.is_decidedly_dragging())
                    && ui.rect_contains_pointer(egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(content_width - LABEL_WIDTH, TRACK_HEIGHT),
                    ));
                let t1_bg = if is_text_dnd_hover {
                    egui::Color32::from_rgb(70, 55, 100)
                } else {
                    egui::Color32::from_gray(35)
                };

                let (t1_rect, t1_resp) = ui.allocate_exact_size(
                    egui::vec2(content_width - LABEL_WIDTH, TRACK_HEIGHT),
                    egui::Sense::click(),
                );
                t1_lane_rect = t1_rect;
                ui.painter().rect_filled(t1_rect, 0.0, t1_bg);
                ui.painter().rect_stroke(
                    t1_rect,
                    0.0,
                    egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
                    egui::StrokeKind::Inside,
                );

                for (tci, tc) in state.timeline.title_clips.iter().enumerate() {
                    let x = t1_rect.left() + tc.start_on_track.as_secs_f32() * pps;
                    let w = (tc.duration.as_secs_f32() * pps).max(2.0);
                    let cr = egui::Rect::from_min_size(
                        egui::pos2(x, t1_rect.top()),
                        egui::vec2(w, TRACK_HEIGHT),
                    );
                    if cr.max.x < t1_rect.left() || cr.min.x > t1_rect.right() {
                        continue;
                    }
                    let is_selected = state.selected_title_clip == Some(tci);
                    ui.painter()
                        .rect_filled(cr, 4.0, egui::Color32::from_rgb(200, 150, 50));
                    let label = if tc.text.is_empty() {
                        "(title)"
                    } else {
                        tc.text.as_str()
                    };
                    let clipped_painter = ui.painter().with_clip_rect(cr);
                    clipped_painter.text(
                        cr.left_center() + egui::vec2(4.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::FontId::proportional(11.0),
                        egui::Color32::WHITE,
                    );
                    if is_selected {
                        ui.painter().rect_stroke(
                            cr,
                            4.0,
                            egui::Stroke::new(2.0, egui::Color32::WHITE),
                            egui::StrokeKind::Outside,
                        );
                    }
                    let clip_id = egui::Id::new(("t1_clip", tci));
                    let clip_resp = ui.interact(cr, clip_id, egui::Sense::click_and_drag());
                    if clip_resp.clicked() {
                        new_title_selection = Some(tci);
                    }

                    // Resize cursor near trim edges
                    let near_trim_edge = clip_resp.hovered()
                        && ui.input(|i| i.pointer.latest_pos()).is_some_and(|ptr| {
                            ptr.x <= x + TRIM_HANDLE_PX || ptr.x >= x + w - TRIM_HANDLE_PX
                        });
                    if near_trim_edge
                        || active_title_trim
                            .as_ref()
                            .is_some_and(|t| t.clip_idx == tci)
                    {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }

                    if clip_resp.drag_started() {
                        // Auto-pause so the user can resize while the playhead stays.
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
                        let ptr_x = ui
                            .input(|i| i.pointer.press_origin())
                            .map(|p| p.x)
                            .unwrap_or(x);
                        if ptr_x <= x + TRIM_HANDLE_PX {
                            new_title_trim = Some(state::TitleClipTrimDrag {
                                clip_idx: tci,
                                edge: state::TrimEdge::Left,
                            });
                        } else if ptr_x >= x + w - TRIM_HANDLE_PX {
                            new_title_trim = Some(state::TitleClipTrimDrag {
                                clip_idx: tci,
                                edge: state::TrimEdge::Right,
                            });
                        } else {
                            let grab = ((ptr_x - t1_rect.left()) / pps
                                - tc.start_on_track.as_secs_f32())
                            .max(0.0);
                            new_title_drag = Some(state::TitleClipDrag {
                                clip_idx: tci,
                                grab_offset_secs: grab,
                            });
                        }
                    }
                    if clip_resp.drag_stopped()
                        && let Some(ref trim) = active_title_trim
                        && trim.clip_idx == tci
                    {
                        if let Some(ptr) = ui.input(|i| i.pointer.latest_pos()) {
                            const MIN_DUR_SECS: f32 = 0.1;
                            match trim.edge {
                                state::TrimEdge::Right => {
                                    let new_w = (ptr.x - x).max(MIN_DUR_SECS * pps);
                                    let new_dur =
                                        Duration::from_secs_f32((new_w / pps).max(MIN_DUR_SECS));
                                    pending_title_trims.push((tci, tc.start_on_track, new_dur));
                                }
                                state::TrimEdge::Left => {
                                    let max_start_x = x + w - MIN_DUR_SECS * pps;
                                    let new_x = ptr.x.min(max_start_x).max(t1_rect.left());
                                    let new_start = Duration::from_secs_f32(
                                        ((new_x - t1_rect.left()) / pps).max(0.0),
                                    );
                                    let old_end = tc.start_on_track + tc.duration;
                                    let new_dur = old_end
                                        .saturating_sub(new_start)
                                        .max(Duration::from_secs_f32(MIN_DUR_SECS));
                                    pending_title_trims.push((tci, new_start, new_dur));
                                }
                            }
                        }
                        clear_title_trim = true;
                    }
                    if clip_resp.drag_stopped()
                        && let Some(ref drag) = active_title_drag
                        && drag.clip_idx == tci
                    {
                        if let Some(ptr) = ui.input(|i| i.pointer.latest_pos()) {
                            let raw_start =
                                ((ptr.x - t1_rect.left()) / pps - drag.grab_offset_secs).max(0.0);
                            pending_title_moves.push((tci, raw_start));
                        }
                        clear_title_drag = true;
                    }
                    clip_resp.context_menu(|ui| {
                        if ui.button("Delete").clicked() {
                            delete_title_clip = Some(tci);
                            ui.close();
                        }
                    });
                }

                // Receive text preset drops from the browser.
                if let Some(payload) = t1_resp.dnd_release_payload::<state::TextClipDragIdx>() {
                    let ptr_x = ui
                        .input(|i| i.pointer.latest_pos().map(|p| p.x))
                        .unwrap_or(t1_rect.left());
                    let start_secs = ((ptr_x - t1_rect.left()) / pps).max(0.0);
                    pending_title_drops.push((payload.0, start_secs));
                }
            });

            // ── Track lanes ────────────────────────────────────────────────────
            for (track_idx, track) in state.timeline.tracks.iter().enumerate() {
                ui.horizontal(|ui| {
                    // Track label + M/S buttons
                    ui.allocate_ui_with_layout(
                        egui::vec2(LABEL_WIDTH, TRACK_HEIGHT),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            let label = if track.kind == state::TrackKind::Video {
                                let vn = state.timeline.tracks[..=track_idx]
                                    .iter()
                                    .filter(|t| t.kind == state::TrackKind::Video)
                                    .count();
                                format!("V{vn}")
                            } else {
                                let an = state.timeline.tracks[..=track_idx]
                                    .iter()
                                    .filter(|t| t.kind == state::TrackKind::Audio)
                                    .count();
                                format!("A{an}")
                            };
                            let kind_color = if track.kind == state::TrackKind::Video {
                                egui::Color32::from_rgb(120, 180, 210)
                            } else {
                                egui::Color32::from_rgb(130, 200, 140)
                            };
                            ui.label(egui::RichText::new(label).strong().color(kind_color));
                            ui.horizontal(|ui| {
                                let m_col = if track.muted {
                                    egui::Color32::from_rgb(240, 160, 40)
                                } else {
                                    egui::Color32::GRAY
                                };
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("M").color(m_col).small(),
                                    ))
                                    .on_hover_text("Mute")
                                    .clicked()
                                {
                                    pending_mute_toggle = Some(track_idx);
                                }
                                let s_col = if track.soloed {
                                    egui::Color32::from_rgb(255, 220, 0)
                                } else {
                                    egui::Color32::GRAY
                                };
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("S").color(s_col).small(),
                                    ))
                                    .on_hover_text("Solo")
                                    .clicked()
                                {
                                    pending_solo_toggle = Some(track_idx);
                                }
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
                                let y_off = ptr.y - t1_lane_rect.bottom();
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
                        state::TrackKind::Video => {
                            egui::Color32::from_rgb(70, 130, 180) // steel blue
                        }
                        state::TrackKind::Audio => {
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
                            let orig_w = eff_dur.as_secs_f32() / tc.speed * pps;
                            let mut new_speed_pct = tc.speed * 100.0;

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

                                // Filmstrip thumbnails — video tracks only
                                if track.kind != state::TrackKind::Audio
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

                                // Waveform — audio tracks only
                                if track.kind == state::TrackKind::Audio
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
                                #[allow(clippy::float_cmp)]
                                let speed_label = if tc.speed != 1.0 {
                                    format!(" [{:.0}%]", tc.speed * 100.0)
                                } else {
                                    String::new()
                                };
                                ui.painter().text(
                                    cr.left_center() + egui::vec2(4.0, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    format!("{}{}", name.as_ref(), speed_label),
                                    egui::FontId::proportional(11.0),
                                    egui::Color32::WHITE,
                                );

                                // Keyframe lane (industry-standard: Premiere/FCP/CapCut
                                // show diamonds at each key's TIME on a strip inside the
                                // clip). Because diamonds are positioned per key-time, a
                                // cut clip only shows the keys that belong to it. FCP-style
                                // double diamond marks a time where >1 property is keyed.
                                if tc.animation.is_active() {
                                    let lane_h = 7.0_f32;
                                    let lane = egui::Rect::from_min_max(
                                        egui::pos2(cr.left(), cr.bottom() - lane_h),
                                        egui::pos2(cr.right(), cr.bottom()),
                                    )
                                    .intersect(cr);
                                    let clipped =
                                        ui.painter().with_clip_rect(lane.intersect(lane_rect));
                                    clipped.rect_filled(
                                        lane,
                                        0.0,
                                        egui::Color32::from_black_alpha(140),
                                    );
                                    let cy = lane.center().y;
                                    // Group key times (ms) across all animated properties.
                                    let mut counts: std::collections::BTreeMap<i64, u32> =
                                        std::collections::BTreeMap::new();
                                    for (_, track) in tc.animation.active_tracks() {
                                        for k in &track.keys {
                                            *counts
                                                .entry((k.t_secs * 1000.0).round() as i64)
                                                .or_insert(0) += 1;
                                        }
                                    }
                                    let diamond = |cx: f32, r: f32| {
                                        vec![
                                            egui::pos2(cx, cy - r),
                                            egui::pos2(cx + r, cy),
                                            egui::pos2(cx, cy + r),
                                            egui::pos2(cx - r, cy),
                                        ]
                                    };
                                    for (ms, count) in counts {
                                        let x = cr.left() + (ms as f32 / 1000.0) * pps;
                                        clipped.add(egui::Shape::convex_polygon(
                                            diamond(x, 3.5),
                                            egui::Color32::WHITE,
                                            egui::Stroke::new(1.0, egui::Color32::BLACK),
                                        ));
                                        if count > 1 {
                                            clipped.add(egui::Shape::convex_polygon(
                                                diamond(x, 5.5),
                                                egui::Color32::TRANSPARENT,
                                                egui::Stroke::new(1.0, egui::Color32::WHITE),
                                            ));
                                        }
                                    }
                                }

                                // Silence region overlays — audio tracks only
                                if track.kind == state::TrackKind::Audio {
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

                                // Fade ramps — audio tracks only. Painted before interaction widgets
                                // so the triangular overlays appear under any interactive chrome.
                                // avio gap: per-clip fade-in/out not applied during render
                                if track.kind == state::TrackKind::Audio {
                                    let clipped = ui.painter().with_clip_rect(cr);
                                    let fade_color =
                                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140);
                                    let line_color =
                                        egui::Color32::from_rgba_unmultiplied(180, 220, 255, 220);

                                    // Fade-in triangle
                                    if tc.fade_in > Duration::ZERO {
                                        let fi_px =
                                            (tc.fade_in.as_secs_f32() * pps).min(cr.width());
                                        let x0 = cr.left();
                                        let x1 = (cr.left() + fi_px).min(cr.right());
                                        clipped.add(egui::Shape::convex_polygon(
                                            vec![
                                                egui::pos2(x0, cr.top()),
                                                egui::pos2(x1, cr.top()),
                                                egui::pos2(x0, cr.bottom()),
                                            ],
                                            fade_color,
                                            egui::Stroke::NONE,
                                        ));
                                        clipped.line_segment(
                                            [egui::pos2(x0, cr.bottom()), egui::pos2(x1, cr.top())],
                                            egui::Stroke::new(1.5, line_color),
                                        );
                                    }

                                    // Fade-out triangle
                                    if tc.fade_out > Duration::ZERO {
                                        let fo_px =
                                            (tc.fade_out.as_secs_f32() * pps).min(cr.width());
                                        let x1 = cr.right();
                                        let x0 = (cr.right() - fo_px).max(cr.left());
                                        clipped.add(egui::Shape::convex_polygon(
                                            vec![
                                                egui::pos2(x0, cr.top()),
                                                egui::pos2(x1, cr.top()),
                                                egui::pos2(x1, cr.bottom()),
                                            ],
                                            fade_color,
                                            egui::Stroke::NONE,
                                        ));
                                        clipped.line_segment(
                                            [egui::pos2(x0, cr.top()), egui::pos2(x1, cr.bottom())],
                                            egui::Stroke::new(1.5, line_color),
                                        );
                                    }
                                }

                                // Sprite frame tooltip on hover + drag-to-reposition/trim + context menu
                                // Registered first so the gain interaction (below) has higher priority.
                                let clip_id = egui::Id::new(("tl_clip", track_idx, clip_i));
                                let clip_resp =
                                    ui.interact(cr, clip_id, egui::Sense::click_and_drag());

                                // Hover popup: which keyframe animations this clip carries.
                                if tc.animation.is_active() {
                                    clip_resp.clone().on_hover_ui(|ui| {
                                        ui.strong("◆ Keyframe Animation");
                                        for (label, track) in tc.animation.active_tracks() {
                                            ui.label(format!(
                                                "{label} ({} keys)",
                                                track.keys.len()
                                            ));
                                            let n = track.keys.len();
                                            for (i, k) in track.keys.iter().enumerate() {
                                                // Opacity values are 0..1 → show as %.
                                                let val = if label == "Opacity" {
                                                    format!("{:.0}%", k.value * 100.0)
                                                } else {
                                                    format!("{:.2}", k.value)
                                                };
                                                let ease = if i + 1 < n {
                                                    format!("  →{}", k.easing.label())
                                                } else {
                                                    String::new()
                                                };
                                                ui.weak(format!("  {:.2}s  {val}{ease}", k.t_secs));
                                            }
                                        }
                                    });
                                }

                                // Cursor change and edge-proximity flag for trim handles
                                let near_trim_edge = clip_resp.hovered()
                                    && ui.input(|i| i.pointer.latest_pos()).is_some_and(|ptr| {
                                        ptr.x <= orig_x + TRIM_HANDLE_PX
                                            || ptr.x >= orig_x + orig_w - TRIM_HANDLE_PX
                                    });
                                if near_trim_edge {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                                }

                                // Gain line — audio tracks only. Registered AFTER clip_resp so it
                                // wins hover/drag priority when the pointer is over the line.
                                // Range: −40 dB (bottom) to +12 dB (top); 0 dB at mid-height.
                                // avio gap: per-clip gain not applied (no audio_filter() on TimelineBuilder)
                                let gain_resp_for_clip = if track.kind == state::TrackKind::Audio {
                                    const GAIN_DB_MAX: f32 = 12.0;
                                    const GAIN_DB_MIN: f32 = -40.0;
                                    let y_frac = if tc.gain_db >= 0.0 {
                                        0.5 * (1.0 - tc.gain_db / GAIN_DB_MAX)
                                    } else {
                                        0.5 + 0.5 * (-tc.gain_db / -GAIN_DB_MIN)
                                    };
                                    let gain_y = cr.top() + y_frac * cr.height();
                                    let gain_hit = egui::Rect::from_center_size(
                                        egui::pos2(cr.center().x, gain_y),
                                        egui::vec2(cr.width(), 8.0),
                                    );
                                    let gain_id = egui::Id::new(("gain_line", track_idx, clip_i));
                                    let gain_resp =
                                        ui.interact(gain_hit, gain_id, egui::Sense::drag());

                                    if gain_resp.dragged() || gain_resp.hovered() {
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                                    }
                                    if gain_resp.dragged()
                                        && let Some(ptr_y) =
                                            ui.input(|i| i.pointer.latest_pos()).map(|p| p.y)
                                    {
                                        let frac =
                                            ((ptr_y - cr.top()) / cr.height()).clamp(0.0, 1.0);
                                        let new_gain = if frac <= 0.5 {
                                            (0.5 - frac) / 0.5 * GAIN_DB_MAX
                                        } else {
                                            -((frac - 0.5) / 0.5) * (-GAIN_DB_MIN)
                                        };
                                        pending_gain.push((track_idx, clip_i, new_gain));
                                    }

                                    let show_label = gain_resp.hovered() || gain_resp.dragged();
                                    let line_color = if show_label {
                                        egui::Color32::from_rgb(255, 220, 80)
                                    } else {
                                        egui::Color32::from_rgba_unmultiplied(200, 180, 60, 180)
                                    };
                                    let clipped = ui.painter().with_clip_rect(cr);
                                    clipped.hline(
                                        cr.x_range(),
                                        gain_y,
                                        egui::Stroke::new(2.0, line_color),
                                    );
                                    if show_label {
                                        clipped.text(
                                            egui::pos2(cr.left() + 4.0, gain_y - 3.0),
                                            egui::Align2::LEFT_BOTTOM,
                                            format!("{:+.1} dB", tc.gain_db),
                                            egui::FontId::monospace(10.0),
                                            egui::Color32::from_rgb(255, 220, 80),
                                        );
                                    }
                                    Some(gain_resp)
                                } else {
                                    None
                                };

                                // Fade handles — audio tracks only. Registered AFTER gain_resp for
                                // the highest interaction priority on the clip rect.
                                let fade_consuming_drag = if track.kind == state::TrackKind::Audio {
                                    const FADE_HANDLE_PX: f32 = 10.0;
                                    let half = cr.height() / 2.0;
                                    let eff_dur_secs = {
                                        let eff_in =
                                            tc.in_point.unwrap_or(Duration::ZERO).as_secs_f32();
                                        let eff_out = tc
                                            .out_point
                                            .unwrap_or(source.info.duration())
                                            .as_secs_f32();
                                        (eff_out - eff_in).max(0.001)
                                    };
                                    let max_fade_secs = eff_dur_secs / 2.0;

                                    // Fade-in handle: small triangle at leading edge
                                    let fi_px = (tc.fade_in.as_secs_f32() * pps).min(cr.width());
                                    let fi_handle_center =
                                        egui::pos2(cr.left() + fi_px, cr.top() + half);
                                    let fi_hit = egui::Rect::from_center_size(
                                        fi_handle_center,
                                        egui::vec2(FADE_HANDLE_PX, cr.height()),
                                    );
                                    let fi_id =
                                        egui::Id::new(("fade_in_handle", track_idx, clip_i));
                                    let fi_resp = ui.interact(fi_hit, fi_id, egui::Sense::drag());
                                    if fi_resp.hovered() || fi_resp.dragged() {
                                        ui.ctx()
                                            .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                                    }
                                    if fi_resp.dragged()
                                        && let Some(ptr_x) =
                                            ui.input(|i| i.pointer.latest_pos()).map(|p| p.x)
                                    {
                                        let new_fi_secs =
                                            ((ptr_x - cr.left()) / pps).max(0.0).min(max_fade_secs);
                                        pending_fades.push((
                                            track_idx,
                                            clip_i,
                                            Some(Duration::from_secs_f32(new_fi_secs)),
                                            None,
                                        ));
                                    }

                                    // Fade-out handle: small triangle at trailing edge
                                    let fo_px = (tc.fade_out.as_secs_f32() * pps).min(cr.width());
                                    let fo_handle_center =
                                        egui::pos2(cr.right() - fo_px, cr.top() + half);
                                    let fo_hit = egui::Rect::from_center_size(
                                        fo_handle_center,
                                        egui::vec2(FADE_HANDLE_PX, cr.height()),
                                    );
                                    let fo_id =
                                        egui::Id::new(("fade_out_handle", track_idx, clip_i));
                                    let fo_resp = ui.interact(fo_hit, fo_id, egui::Sense::drag());
                                    if fo_resp.hovered() || fo_resp.dragged() {
                                        ui.ctx()
                                            .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                                    }
                                    if fo_resp.dragged()
                                        && let Some(ptr_x) =
                                            ui.input(|i| i.pointer.latest_pos()).map(|p| p.x)
                                    {
                                        let new_fo_secs = ((cr.right() - ptr_x) / pps)
                                            .max(0.0)
                                            .min(max_fade_secs);
                                        pending_fades.push((
                                            track_idx,
                                            clip_i,
                                            None,
                                            Some(Duration::from_secs_f32(new_fo_secs)),
                                        ));
                                    }

                                    // Draw handle diamonds so they're visible
                                    let clipped = ui.painter().with_clip_rect(cr);
                                    let handle_color =
                                        egui::Color32::from_rgba_unmultiplied(140, 200, 255, 200);
                                    let dia = 5.0_f32;
                                    for (cx, is_active) in [
                                        (
                                            fi_handle_center.x,
                                            fi_resp.hovered() || fi_resp.dragged(),
                                        ),
                                        (
                                            fo_handle_center.x,
                                            fo_resp.hovered() || fo_resp.dragged(),
                                        ),
                                    ] {
                                        let cy = cr.top() + half;
                                        let color = if is_active {
                                            egui::Color32::WHITE
                                        } else {
                                            handle_color
                                        };
                                        clipped.add(egui::Shape::convex_polygon(
                                            vec![
                                                egui::pos2(cx, cy - dia),
                                                egui::pos2(cx + dia, cy),
                                                egui::pos2(cx, cy + dia),
                                                egui::pos2(cx - dia, cy),
                                            ],
                                            color,
                                            egui::Stroke::NONE,
                                        ));
                                    }

                                    // Show labels when hovered
                                    if fi_resp.hovered() || fi_resp.dragged() {
                                        let clipped2 = ui.painter().with_clip_rect(cr);
                                        clipped2.text(
                                            egui::pos2(cr.left() + fi_px + 6.0, cr.top() + 2.0),
                                            egui::Align2::LEFT_TOP,
                                            format!("FI {:.2}s", tc.fade_in.as_secs_f32()),
                                            egui::FontId::monospace(9.0),
                                            egui::Color32::from_rgb(140, 200, 255),
                                        );
                                    }
                                    if fo_resp.hovered() || fo_resp.dragged() {
                                        let clipped2 = ui.painter().with_clip_rect(cr);
                                        clipped2.text(
                                            egui::pos2(cr.right() - fo_px - 6.0, cr.top() + 2.0),
                                            egui::Align2::RIGHT_TOP,
                                            format!("FO {:.2}s", tc.fade_out.as_secs_f32()),
                                            egui::FontId::monospace(9.0),
                                            egui::Color32::from_rgb(140, 200, 255),
                                        );
                                    }

                                    fi_resp.drag_started()
                                        || fi_resp.dragged()
                                        || fo_resp.drag_started()
                                        || fo_resp.dragged()
                                } else {
                                    false
                                };

                                let gain_consuming_drag = gain_resp_for_clip
                                    .as_ref()
                                    .is_some_and(|r| r.drag_started() || r.dragged());
                                if clip_resp.drag_started()
                                    && !gain_consuming_drag
                                    && !fade_consuming_drag
                                {
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
                                                        let min_right =
                                                            orig_x + one_frame_sec * pps;
                                                        let snapped = snap_trim_edge(
                                                            ptr.x,
                                                            track_idx,
                                                            clip_i,
                                                            &state.timeline.tracks,
                                                            &state.clips,
                                                            20.0,
                                                            lane_rect.left(),
                                                            pps,
                                                        );
                                                        let new_right =
                                                            snapped.clamp(min_right, max_right);
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
                                                        let snapped = snap_trim_edge(
                                                            ptr.x,
                                                            track_idx,
                                                            clip_i,
                                                            &state.timeline.tracks,
                                                            &state.clips,
                                                            20.0,
                                                            lane_rect.left(),
                                                            pps,
                                                        );
                                                        let new_left =
                                                            snapped.clamp(min_left, max_left);
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
                                            // Track lanes start below the T1 title lane, not at
                                            // the ruler — offset by the T1 lane so the drop maps
                                            // to the correct track.
                                            let y_off = ptr.y - t1_lane_rect.bottom();
                                            let dst_track = ((y_off / TRACK_HEIGHT).floor()
                                                as isize)
                                                .clamp(0, tracks_count as isize - 1)
                                                as usize;
                                            let raw_start = ((ptr.x - lane_rect.left()) / pps
                                                - drag.grab_offset_secs)
                                                .max(0.0);
                                            let new_start = snap_clip_start(
                                                raw_start,
                                                eff_dur.as_secs_f32(),
                                                dst_track,
                                                drag.src_track,
                                                drag.src_clip,
                                                &state.timeline.tracks,
                                                &state.clips,
                                                20.0,
                                                pps,
                                            );
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
                                        if track_idx == 0 {
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
                                        // Speed — all tracks
                                        ui.separator();
                                        ui.label("Speed:");
                                        if ui
                                            .add(
                                                egui::DragValue::new(&mut new_speed_pct)
                                                    .range(10.0..=400.0)
                                                    .speed(1.0)
                                                    .suffix(" %"),
                                            )
                                            .changed()
                                        {
                                            pending_speeds.push((
                                                track_idx,
                                                clip_i,
                                                (new_speed_pct / 100.0).clamp(0.1, 4.0),
                                            ));
                                        }
                                        // Delete options — all tracks
                                        ui.separator();
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

                                // White outline on the selected clip.
                                if state.timeline_selected == Some((track_idx, clip_i)) {
                                    ui.painter().rect_stroke(
                                        cr,
                                        4.0,
                                        egui::Stroke::new(2.0, egui::Color32::WHITE),
                                        egui::StrokeKind::Outside,
                                    );
                                }

                                // Left-click (not drag) selects this clip.
                                if clip_resp.clicked() {
                                    new_selection = Some((track_idx, clip_i));
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
                                let src_dur = match (tc.in_point, tc.out_point) {
                                    (Some(i), Some(o)) if o > i => (o - i).as_secs_f32(),
                                    (None, Some(o)) => o.as_secs_f32(),
                                    (Some(i), None) => {
                                        s.info.duration().saturating_sub(i).as_secs_f32()
                                    }
                                    _ => s.info.duration().as_secs_f32(),
                                };
                                src_dur / tc.speed + freeze_extra_secs(tc.freeze)
                            })
                        })
                        .unwrap_or(1.0);

                    // Track lanes start below the T1 title lane (see drop handler).
                    let tracks_top = t1_lane_rect.bottom();
                    let y_off = ptr.y - tracks_top;
                    let dst_ti = ((y_off / TRACK_HEIGHT).floor() as isize)
                        .clamp(0, tracks_count as isize - 1)
                        as usize;

                    let raw_start_secs =
                        ((ptr.x - timeline_left) / pps - drag.grab_offset_secs).max(0.0);
                    let snapped_start = snap_clip_start(
                        raw_start_secs,
                        ghost_dur,
                        dst_ti,
                        drag.src_track,
                        drag.src_clip,
                        &state.timeline.tracks,
                        &state.clips,
                        20.0,
                        pps,
                    );
                    let is_snapping = (snapped_start - raw_start_secs).abs() > 0.001;
                    let ghost_left = timeline_left + snapped_start * pps;
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
                    let snap_color = if is_snapping {
                        egui::Color32::from_rgb(255, 200, 0)
                    } else {
                        egui::Color32::WHITE
                    };
                    ui.painter().rect_stroke(
                        ghost_rect,
                        4.0,
                        egui::Stroke::new(1.5, snap_color),
                        egui::StrokeKind::Outside,
                    );
                }
            }

            // ── T1 ghost clip while dragging title ──────────────────────────────
            if let Some(ref drag) = active_title_drag
                && let Some(ptr) = ui.input(|i| i.pointer.latest_pos())
                && let Some(tc) = state.timeline.title_clips.get(drag.clip_idx)
            {
                let ghost_dur = tc.duration.as_secs_f32();
                let ghost_start =
                    ((ptr.x - t1_lane_rect.left()) / pps - drag.grab_offset_secs).max(0.0);
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(t1_lane_rect.left() + ghost_start * pps, t1_lane_rect.top()),
                    egui::vec2((ghost_dur * pps).max(2.0), TRACK_HEIGHT),
                );
                ui.painter().rect_filled(
                    ghost_rect,
                    4.0,
                    egui::Color32::from_rgba_premultiplied(200, 150, 50, 120),
                );
                ui.painter().rect_stroke(
                    ghost_rect,
                    4.0,
                    egui::Stroke::new(1.5, egui::Color32::WHITE),
                    egui::StrokeKind::Outside,
                );
            }
            // ── T1 trim ghost ────────────────────────────────────────────────────
            if let Some(ref trim) = active_title_trim
                && let Some(ptr) = ui.input(|i| i.pointer.latest_pos())
                && let Some(tc) = state.timeline.title_clips.get(trim.clip_idx)
            {
                const MIN_DUR_SECS: f32 = 0.1;
                let orig_x = t1_lane_rect.left() + tc.start_on_track.as_secs_f32() * pps;
                let orig_w = tc.duration.as_secs_f32() * pps;
                let (ghost_left, ghost_w) = match trim.edge {
                    state::TrimEdge::Right => {
                        let new_w = (ptr.x - orig_x).max(MIN_DUR_SECS * pps);
                        (orig_x, new_w)
                    }
                    state::TrimEdge::Left => {
                        let max_start_x = orig_x + orig_w - MIN_DUR_SECS * pps;
                        let new_x = ptr.x.min(max_start_x).max(t1_lane_rect.left());
                        (new_x, orig_x + orig_w - new_x)
                    }
                };
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(ghost_left, t1_lane_rect.top()),
                    egui::vec2(ghost_w.max(2.0), TRACK_HEIGHT),
                );
                ui.painter().rect_filled(
                    ghost_rect,
                    4.0,
                    egui::Color32::from_rgba_premultiplied(200, 150, 50, 120),
                );
                ui.painter().rect_stroke(
                    ghost_rect,
                    4.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 200, 0)),
                    egui::StrokeKind::Outside,
                );
            }

            // ── Playhead ────────────────────────────────────────────────────────
            let playhead_x = timeline_left + state.timeline_playhead_secs as f32 * pps;
            // Span the ruler, the T1 title lane, and every track lane below it.
            let tracks_bottom =
                t1_lane_rect.bottom() + TRACK_HEIGHT * state.timeline.tracks.len() as f32;
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

    // Apply timeline selection change.
    if let Some(sel) = new_selection {
        state.timeline_selected = Some(sel);
        state.selected_title_clip = None;
    }

    // Apply T1 title clip actions.
    if let Some(idx) = new_title_selection {
        state.selected_title_clip = Some(idx);
        state.timeline_selected = None;
    }
    for (preset_idx, start_secs) in pending_title_drops {
        if let Some(preset) = state.text_presets.get(preset_idx) {
            let start = Duration::from_secs_f32(start_secs);
            let new_tc = state::TitleClip {
                start_on_track: start,
                duration: Duration::from_secs_f32(preset.default_duration_secs),
                text: preset.text.clone(),
                font_size: preset.font_size,
                color: preset.color,
                h_align: preset.h_align,
                v_align: preset.v_align,
            };
            state.timeline.title_clips.push(new_tc);
            state.timeline.title_clips.sort_by_key(|c| c.start_on_track);
            if let Some(idx) = state
                .timeline
                .title_clips
                .iter()
                .position(|c| c.start_on_track == start)
            {
                state.selected_title_clip = Some(idx);
                state.timeline_selected = None;
            }
        }
    }
    if let Some(idx) = delete_title_clip
        && idx < state.timeline.title_clips.len()
    {
        state.timeline.title_clips.remove(idx);
        match state.selected_title_clip {
            Some(sel) if sel == idx => state.selected_title_clip = None,
            Some(sel) if sel > idx => state.selected_title_clip = Some(sel - 1),
            _ => {}
        }
    }

    // Apply title clip moves.
    let had_title_moves = !pending_title_moves.is_empty();
    let title_clips_before = state.timeline.title_clips.clone();
    for (clip_idx, new_start_secs) in pending_title_moves {
        if let Some(tc) = state.timeline.title_clips.get_mut(clip_idx) {
            tc.start_on_track = Duration::from_secs_f32(new_start_secs);
        }
        state.timeline.title_clips.sort_by_key(|c| c.start_on_track);
        // Keep selection pointing at the same clip after sort.
        if let Some(tc) = state.timeline.title_clips.get(clip_idx) {
            let start = tc.start_on_track;
            if let Some(new_idx) = state
                .timeline
                .title_clips
                .iter()
                .position(|c| c.start_on_track == start)
            {
                state.selected_title_clip = Some(new_idx);
            }
        }
    }
    if had_title_moves && title_clips_before != state.timeline.title_clips {
        state.push_edit(state::EditCommand::TitleSnapshot {
            before: title_clips_before,
            after: state.timeline.title_clips.clone(),
            label: "Move Title Clip",
        });
    }

    // Apply title clip trims.
    let had_title_trims = !pending_title_trims.is_empty();
    let title_clips_before_trim = state.timeline.title_clips.clone();
    for (clip_idx, new_start, new_dur) in pending_title_trims {
        if let Some(tc) = state.timeline.title_clips.get_mut(clip_idx) {
            tc.start_on_track = new_start;
            tc.duration = new_dur;
        }
        state.timeline.title_clips.sort_by_key(|c| c.start_on_track);
    }
    if had_title_trims && title_clips_before_trim != state.timeline.title_clips {
        state.push_edit(state::EditCommand::TitleSnapshot {
            before: title_clips_before_trim,
            after: state.timeline.title_clips.clone(),
            label: "Trim Title Clip",
        });
    }

    // Copy selected clip to clipboard (Ctrl+C).
    if do_copy
        && let Some((ti, ci)) = state.timeline_selected
        && let Some(clip) = state
            .timeline
            .tracks
            .get(ti)
            .and_then(|t| t.clips.get(ci))
            .cloned()
    {
        state.timeline_clipboard = Some((ti, clip));
    }

    // Populate paste / duplicate inserts (Ctrl+V / Ctrl+D).
    if do_paste && let Some((src_track, clip)) = state.timeline_clipboard.clone() {
        let src_dur = state
            .clips
            .get(clip.source_index)
            .map(|s| s.info.duration())
            .unwrap_or(Duration::ZERO);
        let eff_src_dur = match (clip.in_point, clip.out_point) {
            (Some(i), Some(o)) if o > i => o - i,
            (None, Some(o)) => o,
            (Some(i), None) => src_dur.saturating_sub(i),
            _ => src_dur,
        };
        let eff_dur = eff_src_dur.div_f32(clip.speed);
        let paste_start = clip.start_on_track + eff_dur;
        let mut new_clip = clip;
        new_clip.start_on_track = paste_start;
        pending_inserts.push((src_track, new_clip));
    }
    if do_duplicate
        && let Some((ti, ci)) = state.timeline_selected
        && let Some(clip) = state
            .timeline
            .tracks
            .get(ti)
            .and_then(|t| t.clips.get(ci))
            .cloned()
    {
        let src_dur = state
            .clips
            .get(clip.source_index)
            .map(|s| s.info.duration())
            .unwrap_or(Duration::ZERO);
        let eff_src_dur = match (clip.in_point, clip.out_point) {
            (Some(i), Some(o)) if o > i => o - i,
            (None, Some(o)) => o,
            (Some(i), None) => src_dur.saturating_sub(i),
            _ => src_dur,
        };
        let eff_dur = eff_src_dur.div_f32(clip.speed);
        let dup_start = clip.start_on_track + eff_dur;
        let mut new_clip = clip;
        new_clip.start_on_track = dup_start;
        new_clip.transition = None;
        pending_inserts.push((ti, new_clip));
    }

    // Snapshot all tracks before applying any pending ops.
    let tracks_before: Vec<Vec<state::TimelineClip>> = state
        .timeline
        .tracks
        .iter()
        .map(|t| t.clips.clone())
        .collect();
    // Flags captured before pending vecs are consumed by for-loops.
    let had_trims = !pending_trims.is_empty();
    let had_moves = !pending_moves.is_empty();
    let had_clips = !pending_clips.is_empty();
    let had_transitions = !pending_transitions.is_empty();
    let had_ripple_delete = pending_deletes.iter().any(|d| d.2);
    let had_deletes = !pending_deletes.is_empty();
    let had_paste = !pending_inserts.is_empty() && do_paste;
    let had_duplicate = !pending_inserts.is_empty() && do_duplicate;

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
    if clear_title_drag {
        state.title_clip_drag = None;
    }
    if let Some(ntd) = new_title_drag {
        state.title_clip_drag = Some(ntd);
    }
    if clear_title_trim {
        state.title_clip_trim = None;
    }
    if let Some(ntt) = new_title_trim {
        state.title_clip_trim = Some(ntt);
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

    // Apply gain line drags.
    for (ti, ci, new_gain) in pending_gain {
        if let Some(clip) = state.timeline.tracks[ti].clips.get_mut(ci) {
            clip.gain_db = new_gain.clamp(-40.0, 12.0);
        }
    }

    // Apply fade handle drags.
    for (ti, ci, new_fi, new_fo) in pending_fades {
        if let Some(clip) = state.timeline.tracks[ti].clips.get_mut(ci) {
            if let Some(fi) = new_fi {
                clip.fade_in = fi;
            }
            if let Some(fo) = new_fo {
                clip.fade_out = fo;
            }
        }
    }

    // Apply mute/solo toggles and restart the player if it is currently running.
    let mute_solo_changed = pending_mute_toggle.is_some() || pending_solo_toggle.is_some();
    if let Some(ti) = pending_mute_toggle {
        state.timeline.tracks[ti].muted = !state.timeline.tracks[ti].muted;
    }
    if let Some(ti) = pending_solo_toggle {
        state.timeline.tracks[ti].soloed = !state.timeline.tracks[ti].soloed;
    }
    if mute_solo_changed {
        let is_playing = state
            .timeline_player_thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);
        if is_playing && !state.timeline_is_paused {
            // Restart the player immediately so the new mute/solo state is heard.
            let resume_pos = Duration::from_secs_f64(state.timeline_playhead_secs.max(0.0));
            let aspect_canvas = state.project_aspect.dims();
            state.stop_timeline_player();
            let clips = &state.clips;
            let make_tcd = |tc: &state::TimelineClip| player::TrackClipData {
                path: clips[tc.source_index].path.clone(),
                start_on_track: tc.start_on_track,
                in_point: tc.in_point,
                out_point: tc.out_point,
                transition: tc.transition,
                transition_duration: tc.transition_duration,
                gain_db: tc.gain_db,
                fade_in: tc.fade_in,
                fade_out: tc.fade_out,
                brightness: tc.brightness,
                contrast: tc.contrast,
                saturation: tc.saturation,
                wb_temperature: tc.wb_temperature,
                wb_tint: tc.wb_tint,
                hue_degrees: tc.hue_degrees,
                gamma_r: tc.gamma_r,
                gamma_g: tc.gamma_g,
                gamma_b: tc.gamma_b,
                lut_path: tc.lut_path.clone(),
                speed: tc.speed,
                freeze: tc.freeze,
                opacity: tc.opacity,
                blend_mode: tc.blend_mode,
                position_x: tc.position_x,
                position_y: tc.position_y,
                scale_pct: tc.scale_pct,
                vignette: tc.vignette,
                vignette_x: tc.vignette_x,
                vignette_y: tc.vignette_y,
                width: clips[tc.source_index]
                    .info
                    .primary_video()
                    .map(|v| v.width())
                    .unwrap_or(0),
                height: clips[tc.source_index]
                    .info
                    .primary_video()
                    .map(|v| v.height())
                    .unwrap_or(0),
                curves: tc.curves.clone(),
                wheels: tc.wheels,
                video_effects: tc.video_effects,
                transform: tc.transform,
                overlay: tc.overlay.clone(),
                subtitle: tc.subtitle.clone(),
                keying: tc.keying,
                mask: tc.mask.clone(),
                animation: tc.animation.clone(),
            };
            let tracks = &state.timeline.tracks;
            let audio_start = state.timeline.audio_track_start();
            let video_tracks: Vec<Vec<_>> = (0..audio_start)
                .map(|ti| {
                    if track_is_active(tracks, ti) {
                        tracks[ti].clips.iter().map(make_tcd).collect()
                    } else {
                        vec![]
                    }
                })
                .collect();
            let a1: Vec<_> = if audio_start < tracks.len() && track_is_active(tracks, audio_start) {
                tracks[audio_start].clips.iter().map(make_tcd).collect()
            } else {
                vec![]
            };
            state
                .cpal_rate
                .store(1.0f64.to_bits(), std::sync::atomic::Ordering::Relaxed);
            let (thread, handle_rx) = player::spawn_timeline_player(
                video_tracks,
                a1,
                Arc::clone(&state.frame_handle),
                ctx,
                resume_pos,
                Arc::clone(&state.cpal_rate),
                aspect_canvas,
            );
            state.timeline_player_thread = Some(thread);
            state.timeline_pending_handle_rx = Some(handle_rx);
        } else {
            // Paused or stopped: mark as dirty so Resume rebuilds with correct state.
            state.clips_moved_while_paused = true;
        }
    }

    // Apply timeline clip moves.
    for (src_track, src_clip, dst_track, new_start_secs) in pending_moves {
        if src_track == dst_track {
            if let Some(clip) = state.timeline.tracks[src_track].clips.get_mut(src_clip) {
                clip.start_on_track = Duration::from_secs_f32(new_start_secs);
            }
            // Re-sort so TimelineRunner always sees clips in timeline order.
            state.timeline.tracks[src_track]
                .clips
                .sort_by_key(|c| c.start_on_track);
        } else if src_clip < state.timeline.tracks[src_track].clips.len() {
            let mut clip = state.timeline.tracks[src_track].clips.remove(src_clip);
            clip.start_on_track = Duration::from_secs_f32(new_start_secs);
            // Sorted insert instead of push to maintain timeline order.
            let track = &mut state.timeline.tracks[dst_track].clips;
            let pos = track
                .iter()
                .position(|c| c.start_on_track > clip.start_on_track)
                .unwrap_or(track.len());
            track.insert(pos, clip);
        }
    }
    if had_moves {
        state.timeline_selected = None;
    }

    // Apply drops after the ScrollArea closure to avoid borrow conflicts.
    for (track_idx, clip_idx, start_secs) in pending_clips {
        let (in_pt, out_pt) = state
            .clips
            .get(clip_idx)
            .map(|c| (c.in_point, c.out_point))
            .unwrap_or_default();
        let new_clip = state::TimelineClip {
            source_index: clip_idx,
            start_on_track: Duration::from_secs_f32(start_secs),
            in_point: in_pt,
            out_point: out_pt,
            transition: None,
            transition_duration: Duration::from_millis(500),
            gain_db: 0.0,
            fade_in: Duration::ZERO,
            fade_out: Duration::ZERO,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            speed: 1.0,
            reverse: false,
            freeze: None,
            opacity: 1.0,
            blend_mode: avio::BlendMode::Normal,
            position_x: 0.0,
            position_y: 0.0,
            scale_pct: 100.0,
            lut_path: None,
            wb_temperature: state::WB_NEUTRAL_TEMP,
            wb_tint: 0.0,
            hue_degrees: 0.0,
            gamma_r: 1.0,
            gamma_g: 1.0,
            gamma_b: 1.0,
            vignette: 0.0,
            vignette_x: 50.0,
            vignette_y: 50.0,
            curves: state::ToneCurves::default(),
            wheels: state::ColorWheels::default(),
            video_effects: state::VideoEffects::default(),
            transform: state::Transform::default(),
            overlay: state::Overlay::default(),
            subtitle: state::Subtitle::default(),
            keying: state::Keying::default(),
            mask: state::Mask::default(),
            animation: state::ClipAnimation::default(),
        };
        // Sorted insert so that out-of-order drops don't corrupt array order.
        let track = &mut state.timeline.tracks[track_idx].clips;
        let pos = track
            .iter()
            .position(|c| c.start_on_track > new_clip.start_on_track)
            .unwrap_or(track.len());
        track.insert(pos, new_clip);
    }
    for (track_idx, clip_i, transition, duration) in pending_transitions {
        if let Some(clip) = state.timeline.tracks[track_idx].clips.get_mut(clip_i) {
            clip.transition = transition;
            clip.transition_duration = duration;
        }
    }
    for (ti, ci, new_speed) in pending_speeds {
        if let Some(clip) = state.timeline.tracks[ti].clips.get_mut(ci) {
            clip.speed = new_speed.clamp(0.1, 4.0);
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
                let eff_src_dur = match (deleted.in_point, deleted.out_point) {
                    (Some(i), Some(o)) if o > i => o - i,
                    (None, Some(o)) => o,
                    (Some(i), None) => src_dur.saturating_sub(i),
                    _ => src_dur,
                };
                let eff_dur = eff_src_dur.div_f32(deleted.speed);
                let gap_start = deleted.start_on_track + eff_dur;
                for clip in &mut state.timeline.tracks[ti].clips {
                    if clip.start_on_track >= gap_start {
                        clip.start_on_track = clip.start_on_track.saturating_sub(eff_dur);
                    }
                }
            }
        }
    }
    if had_deletes {
        state.timeline_selected = None;
    }

    // Apply paste / duplicate inserts, maintaining start_on_track order.
    for (ti, clip) in pending_inserts {
        let track = &mut state.timeline.tracks[ti].clips;
        let pos = track
            .iter()
            .position(|c| c.start_on_track > clip.start_on_track)
            .unwrap_or(track.len());
        track.insert(pos, clip);
    }
    if (had_paste || had_duplicate) && state.timeline_is_paused {
        state.clips_moved_while_paused = true;
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
            let orig_fade_out = state.timeline.tracks[ti].clips[ci].fade_out;
            state.timeline.tracks[ti].clips[ci].fade_out = Duration::ZERO;
            let right = state::TimelineClip {
                source_index,
                start_on_track: right_start,
                in_point: Some(left_out),
                out_point: right_out,
                transition: None,
                transition_duration,
                gain_db: state.timeline.tracks[ti].clips[ci].gain_db,
                fade_in: Duration::ZERO,
                fade_out: orig_fade_out,
                brightness: state.timeline.tracks[ti].clips[ci].brightness,
                contrast: state.timeline.tracks[ti].clips[ci].contrast,
                saturation: state.timeline.tracks[ti].clips[ci].saturation,
                speed: state.timeline.tracks[ti].clips[ci].speed,
                reverse: state.timeline.tracks[ti].clips[ci].reverse,
                freeze: None,
                opacity: state.timeline.tracks[ti].clips[ci].opacity,
                blend_mode: state.timeline.tracks[ti].clips[ci].blend_mode,
                position_x: state.timeline.tracks[ti].clips[ci].position_x,
                position_y: state.timeline.tracks[ti].clips[ci].position_y,
                scale_pct: state.timeline.tracks[ti].clips[ci].scale_pct,
                lut_path: state.timeline.tracks[ti].clips[ci].lut_path.clone(),
                wb_temperature: state.timeline.tracks[ti].clips[ci].wb_temperature,
                wb_tint: state.timeline.tracks[ti].clips[ci].wb_tint,
                hue_degrees: state.timeline.tracks[ti].clips[ci].hue_degrees,
                gamma_r: state.timeline.tracks[ti].clips[ci].gamma_r,
                gamma_g: state.timeline.tracks[ti].clips[ci].gamma_g,
                gamma_b: state.timeline.tracks[ti].clips[ci].gamma_b,
                vignette: state.timeline.tracks[ti].clips[ci].vignette,
                vignette_x: state.timeline.tracks[ti].clips[ci].vignette_x,
                vignette_y: state.timeline.tracks[ti].clips[ci].vignette_y,
                curves: state.timeline.tracks[ti].clips[ci].curves.clone(),
                wheels: state.timeline.tracks[ti].clips[ci].wheels,
                video_effects: state.timeline.tracks[ti].clips[ci].video_effects,
                transform: state.timeline.tracks[ti].clips[ci].transform,
                overlay: state.timeline.tracks[ti].clips[ci].overlay.clone(),
                subtitle: state.timeline.tracks[ti].clips[ci].subtitle.clone(),
                keying: state.timeline.tracks[ti].clips[ci].keying,
                mask: state.timeline.tracks[ti].clips[ci].mask.clone(),
                animation: state.timeline.tracks[ti].clips[ci].animation.clone(),
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
        } else if had_paste {
            "Paste Clip"
        } else if had_duplicate {
            "Duplicate Clip"
        } else if had_clips {
            "Add Clip"
        } else {
            ""
        };
        if !label.is_empty() {
            let snapshots: Vec<_> = (0..state.timeline.tracks.len())
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

    if do_loop_in {
        state.export_in = Some(std::time::Duration::from_secs_f64(
            state.timeline_playhead_secs,
        ));
    }
    if do_loop_out {
        state.export_out = Some(std::time::Duration::from_secs_f64(
            state.timeline_playhead_secs,
        ));
    }
}
