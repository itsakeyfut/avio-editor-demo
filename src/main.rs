fn main() -> eframe::Result<()> {
    env_logger::init();
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
struct AvioEditorApp {}

impl eframe::App for AvioEditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
            });

        // 3. Left: Clip Browser
        egui::SidePanel::left("clip_browser")
            .resizable(true)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.heading("Clip Browser");
            });

        // 4. Center: Source Monitor (must be last)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Source Monitor");
        });
    }
}
