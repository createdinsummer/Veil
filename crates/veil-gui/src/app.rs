//! # app —— Veil 桌面应用的状态机与界面
//!
//! 两个界面（[`Screen`]）：
//! - **未解锁**（[`LockScreen`]）：选 `.veil` 文件 + 输密码 → `Container::open`；
//! - **已解锁**（[`UnlockedScreen`]）：展示目录树（P4 后续接入查看器分发）。
//!
//! 渲染层只做「显示 + 收集输入」，所有数据/逻辑都在 `veil-core`。

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use veil_core::container::Container;
use veil_core::index::{TreeNode, build_tree};

use crate::viewer::{ActiveView, ViewerRegistry};

/// 应用顶层：当前处于哪个界面。
pub struct VeilApp {
    screen: Screen,
}

impl Default for VeilApp {
    fn default() -> Self {
        Self { screen: Screen::Locked(LockScreen::default()) }
    }
}

/// 两个界面之一。
enum Screen {
    Locked(LockScreen),
    Unlocked(UnlockedScreen),
}

/// 界面切换意图：在 `match` 借用结束后再统一应用，避免"边借用 self.screen 边改它"。
enum Transition {
    ToUnlocked(UnlockedScreen),
    ToLocked,
}

impl eframe::App for VeilApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 用深色铺满整个窗口背景
        ui.painter()
            .rect_filled(ui.max_rect(), egui::CornerRadius::same(0), ui.visuals().panel_fill);

        // 先渲染当前界面，收集"是否要切换"
        let transition = match &mut self.screen {
            Screen::Locked(lock) => lock.ui(ui).map(Transition::ToUnlocked),
            Screen::Unlocked(unlocked) => unlocked.ui(ui).then_some(Transition::ToLocked),
        };
        // 借用结束后再切换界面
        match transition {
            Some(Transition::ToUnlocked(u)) => self.screen = Screen::Unlocked(u),
            Some(Transition::ToLocked) => self.screen = Screen::Locked(LockScreen::default()),
            None => {}
        }
    }
}

/// 解锁页状态。
#[derive(Default)]
struct LockScreen {
    veil_path: Option<PathBuf>, // 已选中的 .veil 文件
    password: String,           // 密码输入框内容
    error: Option<String>,      // 上次解锁的错误提示
}

impl LockScreen {
    /// 渲染解锁页（居中卡片）。成功解锁则返回 `Some(UnlockedScreen)`。
    fn ui(&mut self, ui: &mut egui::Ui) -> Option<UnlockedScreen> {
        let mut result = None;

        ui.vertical_centered(|ui| {
            ui.add_space(70.0);

            // 一张居中的卡片
            egui::Frame::group(ui.style())
                .fill(ui.visuals().extreme_bg_color)
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::same(28))
                .show(ui, |ui| {
                    ui.set_width(280.0);
                    ui.vertical_centered(|ui| {
                        ui.heading("🔒 Veil 保险箱");
                        ui.add_space(20.0);

                        // 选文件
                        let pick = egui::Button::new("选择 .veil 文件…")
                            .min_size(egui::vec2(220.0, 30.0));
                        if ui.add(pick).clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Veil 容器", &["veil"])
                                .pick_file()
                        {
                            self.veil_path = Some(path);
                            self.error = None;
                        }

                        // 已选文件名（只显示文件名，别把完整路径塞满）
                        let file_label = match &self.veil_path {
                            Some(p) => p
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            None => "未选择文件".to_owned(),
                        };
                        ui.small(file_label);

                        ui.add_space(16.0);
                        // 密码框（掩码 + 占位提示 + 铺满卡片宽度）
                        ui.add(
                            egui::TextEdit::singleline(&mut self.password)
                                .password(true)
                                .hint_text("密码")
                                .desired_width(f32::INFINITY),
                        );

                        ui.add_space(16.0);
                        let can_unlock = self.veil_path.is_some() && !self.password.is_empty();
                        let unlock = egui::Button::new("解锁").min_size(egui::vec2(220.0, 32.0));
                        if ui.add_enabled(can_unlock, unlock).clicked() {
                            result = self.try_unlock();
                        }

                        if let Some(err) = &self.error {
                            ui.add_space(10.0);
                            ui.colored_label(egui::Color32::from_rgb(230, 90, 90), err);
                        }
                    });
                });

            ui.add_space(16.0);
            ui.small("🔑 忘记密码将无法恢复，无后门。");
        });

        result
    }

    /// 尝试用当前路径+密码打开容器。
    fn try_unlock(&mut self) -> Option<UnlockedScreen> {
        let path = self.veil_path.clone()?;
        // 直接传 String（core 接口收 impl Into<SecretString>）；解锁 scrypt 需 1~2 秒属正常
        match Container::open(&path, self.password.clone()) {
            Ok(container) => {
                self.password.clear(); // 别把明文密码留在输入框里
                Some(UnlockedScreen {
                    container,
                    registry: ViewerRegistry::with_defaults(),
                    selected: None,
                    current_view: None,
                    status: None,
                })
            }
            Err(e) => {
                self.error = Some(format!("解锁失败：{e}"));
                None
            }
        }
    }
}

/// 已解锁页状态。
struct UnlockedScreen {
    container: Container,
    registry: ViewerRegistry,
    selected: Option<String>, // 当前选中文件的虚拟路径
    /// 当前应用内视图：(所属文件路径, 视图)。切换文件/关闭时置 None → Drop 清理资源。
    current_view: Option<(String, Box<dyn ActiveView>)>,
    status: Option<String>, // 操作结果提示（如导出成功/失败）
}

impl UnlockedScreen {
    /// 渲染已解锁页（顶部栏 + 左侧目录树 + 右侧详情）。用户点"锁定"则返回 `true`。
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut lock_requested = false;

        // 选中项变了 → 关掉旧视图（RAII 清理临时明文 / 释放纹理）
        if let Some((view_path, _)) = &self.current_view
            && self.selected.as_deref() != Some(view_path.as_str())
        {
            self.current_view = None;
        }

        // 顶部栏
        egui::Panel::top("veil_top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("📁 媒体库");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🔒 锁定").clicked() {
                        lock_requested = true;
                    }
                });
            });
            ui.add_space(6.0);
        });

        // 左侧：目录树（把扁平 nodes 折叠成树来展示）
        let tree = build_tree(self.container.nodes());
        egui::Panel::left("veil_tree")
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if tree.children.is_empty() {
                            ui.weak("(空容器)");
                        } else {
                            show_tree(ui, &tree, &mut self.selected);
                        }
                    });
            });

        // 右侧：选中文件的详情 + 操作
        egui::CentralPanel::default().show(ui, |ui| {
            self.detail_ui(ui);
        });

        lock_requested
    }

    /// 右侧详情面板：文件信息 + 「应用内打开 / 导出」+ 活动视图渲染。
    fn detail_ui(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.selected.clone() else {
            ui.centered_and_justified(|ui| ui.weak("← 在左侧选择一个文件"));
            return;
        };
        // 从目录树里取该文件的元数据
        let Some(node) = self.container.nodes().iter().find(|n| n.path == path).cloned() else {
            return;
        };
        let file_name = node.path.rsplit('/').next().unwrap_or(&node.path).to_owned();

        ui.add_space(8.0);
        ui.heading(&file_name);
        ui.add_space(8.0);
        ui.label(format!("路径：{}", node.path));
        ui.label(format!("大小：{} 字节", node.size));
        ui.label(format!("类型：{}", node.mime.as_deref().unwrap_or("未知")));
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            // 有查看器命中 → 提供「应用内打开」
            if self.registry.find(&node).is_some() && ui.button("👁 应用内打开").clicked() {
                let ctx = ui.ctx().clone();
                match self
                    .registry
                    .find(&node)
                    .unwrap()
                    .open(&self.container, &node, &ctx)
                {
                    Ok(view) => {
                        self.current_view = Some((path.clone(), view));
                        self.status = None;
                    }
                    Err(e) => self.status = Some(format!("打开失败：{e}")),
                }
            }

            // 导出到本地（复用 core 的 extract_file）——始终可用
            if ui.button("💾 导出到本地…").clicked()
                && let Some(dest) = rfd::FileDialog::new().set_file_name(&file_name).save_file()
            {
                self.status = match self.container.extract_file(&path, &dest) {
                    Ok(()) => Some(format!("已导出到 {}", dest.display())),
                    Err(e) => Some(format!("导出失败：{e}")),
                };
            }
        });

        if let Some(s) = &self.status {
            ui.add_space(8.0);
            ui.small(s);
        }

        // 渲染当前活动视图（若属于当前选中文件）
        if let Some((view_path, view)) = self.current_view.as_mut()
            && *view_path == path
        {
            ui.separator();
            view.ui(ui);
        }
    }
}

/// 递归渲染目录树：目录用可折叠标题，文件用可选中标签。
fn show_tree(ui: &mut egui::Ui, node: &TreeNode, selected: &mut Option<String>) {
    for (name, child) in &node.children {
        match &child.file {
            // 文件：可选中
            Some(fs_node) => {
                let is_sel = selected.as_deref() == Some(fs_node.path.as_str());
                if ui.selectable_label(is_sel, format!("📄 {name}")).clicked() {
                    *selected = Some(fs_node.path.clone());
                }
            }
            // 目录：可折叠
            None => {
                egui::CollapsingHeader::new(format!("📁 {name}"))
                    .default_open(true)
                    .show(ui, |ui| show_tree(ui, child, selected));
            }
        }
    }
}

/// 全局样式：深色主题 + 更大字号 + 更松间距。
pub fn setup_style(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};

    // 强制深色主题（不跟随系统亮/暗）
    ctx.set_theme(egui::ThemePreference::Dark);

    // 0.35 的样式是主题感知的：用 all_styles_mut 一次改所有主题的字号/间距
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(26.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(12.5, FontFamily::Proportional)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 10.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
    });
}

/// 给 egui 装一个中文字体，否则中文显示为方块 □。
///
/// 依次尝试几个平台上常见的中文字体路径，用第一个能读到的。都没有就跳过
/// （中文会显示为方块，但不会崩）。
pub fn setup_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        // macOS（优先单体 .ttf，最稳）
        "/Library/Fonts/Arial Unicode.ttf",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        // Linux
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        // Windows
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
    ];

    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return; // 没找到中文字体
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
    // 放到比例字体族的最前（优先用它渲染），等宽字体也追加上
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "cjk".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("cjk".to_owned());

    ctx.set_fonts(fonts);
}
