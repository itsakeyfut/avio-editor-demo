/// Studio-grade dark theme for avio-editor-demo.
///
/// Call once from the eframe CreationContext before the first frame.
pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();

    // ── Panel / window backgrounds ─────────────────────────────────────────
    v.panel_fill = egui::Color32::from_rgb(20, 20, 28);
    v.window_fill = egui::Color32::from_rgb(28, 28, 38);
    v.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(55, 55, 75));
    v.window_corner_radius = egui::CornerRadius::same(6);
    v.menu_corner_radius = egui::CornerRadius::same(4);

    // ── Widget states ──────────────────────────────────────────────────────
    let bg_inactive = egui::Color32::from_rgb(38, 38, 52);
    let bg_hovered = egui::Color32::from_rgb(52, 52, 70);
    let bg_active = egui::Color32::from_rgb(65, 65, 88);
    let border = egui::Color32::from_rgb(65, 65, 88);
    let border_hovered = egui::Color32::from_rgb(100, 140, 210);
    let cr4 = egui::CornerRadius::same(4);

    v.widgets.noninteractive.bg_fill = bg_inactive;
    v.widgets.noninteractive.weak_bg_fill = bg_inactive;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(160));
    v.widgets.noninteractive.corner_radius = cr4;

    v.widgets.inactive.bg_fill = bg_inactive;
    v.widgets.inactive.weak_bg_fill = bg_inactive;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(200));
    v.widgets.inactive.corner_radius = cr4;

    v.widgets.hovered.bg_fill = bg_hovered;
    v.widgets.hovered.weak_bg_fill = bg_hovered;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, border_hovered);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
    v.widgets.hovered.corner_radius = cr4;

    v.widgets.active.bg_fill = bg_active;
    v.widgets.active.weak_bg_fill = bg_active;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, border_hovered);
    v.widgets.active.fg_stroke = egui::Stroke::new(2.0, egui::Color32::WHITE);
    v.widgets.active.corner_radius = cr4;

    v.widgets.open.bg_fill = bg_active;
    v.widgets.open.weak_bg_fill = bg_active;
    v.widgets.open.bg_stroke = egui::Stroke::new(1.0, border_hovered);
    v.widgets.open.fg_stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);
    v.widgets.open.corner_radius = cr4;

    // ── Selection accent (blue) ────────────────────────────────────────────
    v.selection.bg_fill = egui::Color32::from_rgba_premultiplied(60, 110, 200, 180);
    v.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(100, 160, 255));

    // ── Hyperlink / accent colour ──────────────────────────────────────────
    v.hyperlink_color = egui::Color32::from_rgb(100, 160, 255);

    // ── Misc ───────────────────────────────────────────────────────────────
    v.extreme_bg_color = egui::Color32::from_rgb(12, 12, 18);
    v.faint_bg_color = egui::Color32::from_rgb(26, 26, 36);
    v.code_bg_color = egui::Color32::from_rgb(26, 26, 36);
    v.warn_fg_color = egui::Color32::from_rgb(255, 200, 60);
    v.error_fg_color = egui::Color32::from_rgb(220, 80, 80);
    v.window_shadow = egui::Shadow {
        offset: [4, 6],
        blur: 16,
        spread: 0,
        color: egui::Color32::from_black_alpha(120),
    };

    ctx.set_visuals(v);

    // Slightly more comfortable spacing and font sizes
    let mut style = (*ctx.style()).clone();
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(13.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::new(11.0, egui::FontFamily::Proportional),
    );
    style.spacing.button_padding = egui::vec2(8.0, 4.0);
    style.spacing.item_spacing = egui::vec2(8.0, 5.0);
    ctx.set_style(style);
}
