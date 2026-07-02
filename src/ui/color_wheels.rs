//! 3-way colour-corrector wheels (lift / gamma / gain) for Clip Properties.
//!
//! Each wheel is a circular disk with a draggable dot: the dot's position is a
//! colour offset (hue = angle, strength = radius) and a slider below sets the
//! range's luminance. The wheel state is mapped to avio's `FilterStep::ThreeWayCC`
//! (per-channel `Rgb`, neutral `1.0`) in `export::apply_color_grade` — a demo
//! approximation (avio's `ThreeWayCC` is itself a 3-point curves model).

use crate::state::{ColorWheel, ColorWheels};

const WHEEL: f32 = 96.0;

/// One colour wheel: label, draggable disk, luminance slider.
fn color_wheel(ui: &mut egui::Ui, label: &str, w: &mut ColorWheel) {
    ui.vertical(|ui| {
        ui.label(label);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(WHEEL, WHEEL), egui::Sense::click_and_drag());
        let center = rect.center();
        let radius = WHEEL / 2.0 - 4.0;

        // Drag the dot (clamped to the disk); double-click resets the colour.
        if resp.double_clicked() {
            w.x = 0.0;
            w.y = 0.0;
        } else if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let mut d = pos - center;
            let len = d.length();
            if len > radius {
                d *= radius / len;
            }
            w.x = (d.x / radius).clamp(-1.0, 1.0);
            w.y = (-d.y / radius).clamp(-1.0, 1.0);
        }

        {
            let painter = ui.painter_at(rect);
            painter.circle_filled(center, radius, egui::Color32::from_gray(24));
            painter.circle_stroke(
                center,
                radius,
                egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
            );
            let cross = egui::Stroke::new(1.0, egui::Color32::from_gray(45));
            painter.line_segment(
                [
                    egui::pos2(center.x - radius, center.y),
                    egui::pos2(center.x + radius, center.y),
                ],
                cross,
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, center.y - radius),
                    egui::pos2(center.x, center.y + radius),
                ],
                cross,
            );
            let dot = egui::pos2(center.x + w.x * radius, center.y - w.y * radius);
            painter.circle_filled(dot, 4.0, egui::Color32::WHITE);
            painter.circle_stroke(
                dot,
                4.0,
                egui::Stroke::new(1.0, egui::Color32::from_gray(30)),
            );
        }

        ui.spacing_mut().slider_width = WHEEL - 24.0;
        ui.add(egui::Slider::new(&mut w.luma, -1.0..=1.0).fixed_decimals(2));
    });
}

/// Renders the three colour wheels (Lift / Gamma / Gain) plus a reset control.
pub fn color_wheels_editor(ui: &mut egui::Ui, wheels: &mut ColorWheels) {
    ui.horizontal(|ui| {
        color_wheel(ui, "Lift", &mut wheels.lift);
        color_wheel(ui, "Gamma", &mut wheels.gamma);
        color_wheel(ui, "Gain", &mut wheels.gain);
    });
    ui.horizontal(|ui| {
        let neutral = wheels.is_neutral();
        if ui
            .add_enabled(!neutral, egui::Button::new("Reset all"))
            .clicked()
        {
            *wheels = ColorWheels::default();
        }
        ui.label("drag = colour · double-click = reset · slider = luminance");
    });
}
