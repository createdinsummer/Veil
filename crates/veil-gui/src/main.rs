mod app;
mod viewer;

use app::VeilApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([880.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Veil", // 窗口标题
        options,
        // cc 是 CreationContext：窗口/字体就绪后回调造 App
        Box::new(|cc| {
            app::setup_cjk_font(&cc.egui_ctx); // 装中文字体，否则中文显示为方块
            app::setup_style(&cc.egui_ctx); // 深色主题 + 大字号 + 松间距
            Ok(Box::new(VeilApp::default()))
        }),
    )
}
