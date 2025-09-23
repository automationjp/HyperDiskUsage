mod ui;

fn main() {
    env_logger::init();
    #[cfg(feature = "debug-eyre")]
    {
        let _ = color_eyre::install();
    }
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HyperDU GUI")
            .with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "HyperDU GUI",
        native_options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    ) {
        eprintln!("GUI error: {e}");
    }
}
