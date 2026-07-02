//! Draggable tone-curve editor for the Clip Properties panel.
//!
//! Edits one [`ToneCurves`] channel at a time (Luma / R / G / B). Each channel is
//! a list of `(x, y)` control points in `0.0..=1.0`; an empty list is identity
//! (that channel is omitted from the `curves` filter). The drawn line is a linear
//! interpolation of the control points — FFmpeg's `curves` applies a cubic spline,
//! so the preview/export is a smooth approximation of the editor's polyline.

use crate::state::{CurveChannel, ToneCurves};

const CANVAS: f32 = 200.0;
/// Squared pixel radius within which a click counts as "on an existing point".
const HANDLE_HIT: f32 = 9.0;

fn channel_vec(curves: &ToneCurves, ch: CurveChannel) -> &Vec<(f32, f32)> {
    match ch {
        CurveChannel::Luma => &curves.master,
        CurveChannel::R => &curves.r,
        CurveChannel::G => &curves.g,
        CurveChannel::B => &curves.b,
    }
}

fn channel_vec_mut(curves: &mut ToneCurves, ch: CurveChannel) -> &mut Vec<(f32, f32)> {
    match ch {
        CurveChannel::Luma => &mut curves.master,
        CurveChannel::R => &mut curves.r,
        CurveChannel::G => &mut curves.g,
        CurveChannel::B => &mut curves.b,
    }
}

fn channel_color(ch: CurveChannel) -> egui::Color32 {
    match ch {
        CurveChannel::Luma => egui::Color32::from_gray(230),
        CurveChannel::R => egui::Color32::from_rgb(230, 90, 90),
        CurveChannel::G => egui::Color32::from_rgb(90, 210, 90),
        CurveChannel::B => egui::Color32::from_rgb(100, 140, 240),
    }
}

/// Renders the tone-curve editor for `curves`. The active channel is kept in egui
/// temp memory so it persists across frames.
pub fn tone_curve_editor(ui: &mut egui::Ui, curves: &mut ToneCurves) {
    let chan_id = ui.id().with("tone_curve_active_channel");
    let mut active = ui
        .data_mut(|d| d.get_temp::<CurveChannel>(chan_id))
        .unwrap_or(CurveChannel::Luma);

    // ── Channel selector + per-channel reset ──────────────────────────────────
    ui.horizontal(|ui| {
        for (label, ch) in [
            ("Luma", CurveChannel::Luma),
            ("R", CurveChannel::R),
            ("G", CurveChannel::G),
            ("B", CurveChannel::B),
        ] {
            if ui.selectable_label(active == ch, label).clicked() {
                active = ch;
            }
        }
        ui.separator();
        let non_empty = !channel_vec(curves, active).is_empty();
        if ui
            .add_enabled(non_empty, egui::Button::new("Reset"))
            .on_hover_text("Reset this channel to identity")
            .clicked()
        {
            channel_vec_mut(curves, active).clear();
        }
    });
    ui.data_mut(|d| d.insert_temp(chan_id, active));

    // ── Canvas ────────────────────────────────────────────────────────────────
    let (rect, bg_resp) = ui.allocate_exact_size(egui::vec2(CANVAS, CANVAS), egui::Sense::click());

    let to_screen = |p: (f32, f32)| {
        egui::pos2(
            rect.left() + p.0.clamp(0.0, 1.0) * rect.width(),
            rect.bottom() - p.1.clamp(0.0, 1.0) * rect.height(),
        )
    };
    let to_data = |pos: egui::Pos2| {
        (
            ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
            ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0),
        )
    };

    // Materialize the active channel's points (empty ⇒ identity for display/edit).
    let stored = channel_vec(curves, active);
    let mut pts: Vec<(f32, f32)> = if stored.is_empty() {
        vec![(0.0, 0.0), (1.0, 1.0)]
    } else {
        stored.clone()
    };
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));

    // ── Interaction (no painter held here) ────────────────────────────────────
    let mut changed = false;
    let mut remove_idx: Option<usize> = None;
    let mut hovered = vec![false; pts.len()];
    let n = pts.len();
    for i in 0..n {
        let center = to_screen(pts[i]);
        let handle_rect = egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0));
        let resp = ui.interact(
            handle_rect,
            bg_resp.id.with(i),
            egui::Sense::click_and_drag(),
        );
        hovered[i] = resp.hovered();
        let is_endpoint = i == 0 || i == n - 1;
        if resp.dragged() {
            let d = resp.drag_delta();
            let ny = (pts[i].1 + (-d.y) / rect.height()).clamp(0.0, 1.0);
            let nx = if is_endpoint {
                pts[i].0 // endpoints locked in x
            } else {
                (pts[i].0 + d.x / rect.width()).clamp(pts[i - 1].0 + 0.001, pts[i + 1].0 - 0.001)
            };
            pts[i] = (nx, ny);
            changed = true;
        }
        if resp.double_clicked() && !is_endpoint {
            remove_idx = Some(i);
        }
    }
    if let Some(i) = remove_idx {
        pts.remove(i);
        changed = true;
    }

    // Add a point on a background click that is not on an existing handle.
    if bg_resp.clicked()
        && let Some(pos) = bg_resp.interact_pointer_pos()
    {
        let on_handle = pts
            .iter()
            .any(|&p| to_screen(p).distance_sq(pos) < HANDLE_HIT);
        if !on_handle {
            pts.push(to_data(pos));
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            changed = true;
        }
    }

    if changed {
        *channel_vec_mut(curves, active) = pts.clone();
    }

    // ── Paint ─────────────────────────────────────────────────────────────────
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(24));
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
        egui::StrokeKind::Inside,
    );
    // Quarter grid + identity diagonal.
    let grid = egui::Stroke::new(1.0, egui::Color32::from_gray(45));
    for k in 1..4 {
        let f = k as f32 / 4.0;
        painter.line_segment([to_screen((f, 0.0)), to_screen((f, 1.0))], grid);
        painter.line_segment([to_screen((0.0, f)), to_screen((1.0, f))], grid);
    }
    painter.line_segment(
        [to_screen((0.0, 0.0)), to_screen((1.0, 1.0))],
        egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
    );
    // Curve polyline.
    let curve_stroke = egui::Stroke::new(1.6, channel_color(active));
    for w in pts.windows(2) {
        painter.line_segment([to_screen(w[0]), to_screen(w[1])], curve_stroke);
    }
    // Control-point handles.
    for (i, p) in pts.iter().enumerate() {
        let r = if hovered.get(i).copied().unwrap_or(false) {
            5.0
        } else {
            3.5
        };
        painter.circle_filled(to_screen(*p), r, egui::Color32::WHITE);
    }
}
