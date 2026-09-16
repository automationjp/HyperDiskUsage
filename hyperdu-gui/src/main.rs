mod app;
mod fonts;
mod scan;

/// 256x256 RGBA export of assets/hyperdu.ico. winit turns it into the
/// title-bar icon; the taskbar, Alt-Tab and Explorer read the icon that
/// build.rs embeds into the executable instead.
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/hyperdu-256.png");

fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(WINDOW_ICON_PNG).unwrap_or_else(|err| {
        log::warn!("window icon unavailable, using the OS default: {err}");
        egui::IconData::default()
    })
}

fn main() {
    env_logger::init();
    #[cfg(feature = "debug-eyre")]
    {
        let _ = color_eyre::install();
    }
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HyperDU GUI")
            .with_inner_size([1100.0, 720.0])
            .with_icon(window_icon()),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "HyperDU GUI",
        native_options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    ) {
        eprintln!("GUI error: {e}");
    }
}
