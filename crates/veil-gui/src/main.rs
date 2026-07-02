use eframe::egui;
use std::boxed::Box;

fn main() -> eframe::Result<()> { //
    let options = eframe::NativeOptions::default();
    // 创建一个 eframe 应用实例,并运行它
    // 应用实例的 UI 函数会在每次绘制时调用,用于更新应用的 UI 界面
    eframe::run_native(
        "Veil", // 窗口标题
        options, // 刚才的配置(所有权移交给 run_native)
        // 闭包参数 cc 是 CreationContext(能拿字体、存储、GL 上下文等),先不用,等后面再用
        // 创建一个 VeilApp 实例,并返回它
        Box::new(|_cc| Ok(Box::new(VeilApp::default()))),
    )
}

#[derive(Default)]
struct VeilApp;

impl eframe::App for VeilApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("Veil");
    }
}