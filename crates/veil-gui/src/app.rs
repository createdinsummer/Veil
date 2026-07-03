//! # app —— Veil 桌面应用的状态机与界面
//!
//! 两个界面（[`Screen`]）：
//! - **未解锁**（[`LockScreen`]）：新建 / 打开容器；
//! - **已解锁**（[`UnlockedScreen`]）：目录树浏览 + 各种操作（增删/改名/导出/改密码/查看）。
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

enum Screen {
    Locked(LockScreen),
    // Box：UnlockedScreen 比 LockScreen 大得多，装箱避免 enum 整体变大
    Unlocked(Box<UnlockedScreen>),
}

enum Transition {
    ToUnlocked(Box<UnlockedScreen>),
    ToLocked,
}

impl eframe::App for VeilApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.painter()
            .rect_filled(ui.max_rect(), egui::CornerRadius::same(0), ui.visuals().panel_fill);

        let transition = match &mut self.screen {
            Screen::Locked(lock) => lock.ui(ui).map(|u| Transition::ToUnlocked(Box::new(u))),
            Screen::Unlocked(unlocked) => unlocked.ui(ui).then_some(Transition::ToLocked),
        };
        match transition {
            Some(Transition::ToUnlocked(u)) => self.screen = Screen::Unlocked(u),
            Some(Transition::ToLocked) => self.screen = Screen::Locked(LockScreen::default()),
            None => {}
        }
    }
}

// ───────────────────────── 解锁页 ─────────────────────────

#[derive(Default)]
struct LockScreen {
    veil_path: Option<PathBuf>,
    password: String,
    error: Option<String>,
}

impl LockScreen {
    fn ui(&mut self, ui: &mut egui::Ui) -> Option<UnlockedScreen> {
        let mut result = None;

        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            egui::Frame::group(ui.style())
                .fill(ui.visuals().extreme_bg_color)
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::same(28))
                .show(ui, |ui| {
                    ui.set_width(300.0);
                    ui.vertical_centered(|ui| {
                        ui.heading("🔒 Veil 保险箱");
                        ui.add_space(20.0);

                        // 打开已有容器
                        let pick =
                            egui::Button::new("📂 选择 .veil 文件…").min_size(egui::vec2(240.0, 30.0));
                        if ui.add(pick).clicked()
                            && let Some(path) = rfd::FileDialog::new()
                                .add_filter("Veil 容器", &["veil"])
                                .pick_file()
                        {
                            self.veil_path = Some(path);
                            self.error = None;
                        }
                        let file_label = match &self.veil_path {
                            Some(p) => p
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            None => "未选择文件".to_owned(),
                        };
                        ui.small(file_label);

                        ui.add_space(16.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.password)
                                .password(true)
                                .hint_text("密码")
                                .desired_width(f32::INFINITY),
                        );

                        ui.add_space(16.0);
                        let can_unlock = self.veil_path.is_some() && !self.password.is_empty();
                        if ui
                            .add_enabled(can_unlock, egui::Button::new("解锁").min_size(egui::vec2(240.0, 32.0)))
                            .clicked()
                        {
                            result = self.try_unlock();
                        }

                        ui.add_space(6.0);
                        // 新建容器
                        if ui
                            .add(egui::Button::new("🆕 新建保险箱…").min_size(egui::vec2(240.0, 28.0)))
                            .clicked()
                        {
                            result = self.try_create();
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

    fn try_unlock(&mut self) -> Option<UnlockedScreen> {
        let path = self.veil_path.clone()?;
        match Container::open(&path, self.password.clone()) {
            Ok(container) => {
                self.password.clear();
                Some(UnlockedScreen::new(container))
            }
            Err(e) => {
                self.error = Some(format!("解锁失败：{e}"));
                None
            }
        }
    }

    fn try_create(&mut self) -> Option<UnlockedScreen> {
        if self.password.is_empty() {
            self.error = Some("请先输入密码".to_owned());
            return None;
        }
        let path = rfd::FileDialog::new()
            .add_filter("Veil 容器", &["veil"])
            .set_file_name("vault.veil")
            .save_file()?;
        match Container::create(&path, self.password.clone()) {
            Ok(container) => {
                self.password.clear();
                Some(UnlockedScreen::new(container))
            }
            Err(e) => {
                self.error = Some(format!("创建失败：{e}"));
                None
            }
        }
    }
}

// ───────────────────────── 已解锁页 ─────────────────────────

/// 当前选中项：文件或目录。
#[derive(Clone, PartialEq)]
enum Selection {
    File(String),
    Dir(String),
}

struct UnlockedScreen {
    container: Container,
    registry: ViewerRegistry,
    selected: Option<Selection>,
    current_view: Option<(String, Box<dyn ActiveView>)>,
    status: Option<String>,
    rename_buf: String,
    // 改密码对话框
    pw_dialog: bool,
    new_pw: String,
    new_pw2: String,
}

impl UnlockedScreen {
    fn new(container: Container) -> Self {
        Self {
            container,
            registry: ViewerRegistry::with_defaults(),
            selected: None,
            current_view: None,
            status: None,
            rename_buf: String::new(),
            pw_dialog: false,
            new_pw: String::new(),
            new_pw2: String::new(),
        }
    }

    /// 渲染已解锁页。用户点"锁定"则返回 `true`。
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let ctx = ui.ctx().clone();
        let mut lock_requested = false;

        // 顶部工具栏
        egui::Panel::top("veil_top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.heading("📁 媒体库");
                ui.separator();
                if ui.button("➕ 添加文件").clicked() {
                    self.add_files_action();
                }
                if ui.button("📂 添加文件夹").clicked() {
                    self.add_folder_action();
                }
                if ui.button("📤 导出全部").clicked() {
                    self.export_all_action();
                }
                if ui.button("🔑 修改密码").clicked() {
                    self.pw_dialog = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("🔒 锁定").clicked() {
                        lock_requested = true;
                    }
                });
            });
            ui.add_space(6.0);
        });

        // 左侧目录树（收集本帧点击的项，稍后统一应用）
        let tree = build_tree(self.container.nodes());
        let mut clicked: Option<Selection> = None;
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
                            show_tree(ui, &tree, "", &self.selected, &mut clicked);
                        }
                    });
            });

        // 应用点击：**换了选中项就清掉上一项的临时 UI 状态**（操作提示、重命名输入）
        if let Some(sel) = clicked {
            if self.selected.as_ref() != Some(&sel) {
                self.status = None;
                self.rename_buf.clear();
            }
            self.selected = Some(sel);
        }
        // 选中项不再是正在查看的文件 → 关掉视图（RAII 清理临时明文/纹理）
        if let Some((view_path, _)) = &self.current_view {
            let still = matches!(&self.selected, Some(Selection::File(p)) if p == view_path);
            if !still {
                self.current_view = None;
            }
        }

        // 右侧详情
        egui::CentralPanel::default().show(ui, |ui| self.detail_ui(ui));

        // 改密码对话框
        self.password_dialog(&ctx);

        lock_requested
    }

    // ---- 工具栏动作 ----

    fn add_files_action(&mut self) {
        let Some(paths) = rfd::FileDialog::new().pick_files() else {
            return;
        };
        // 若选中了目录，加到该目录下，否则放根
        let prefix = match &self.selected {
            Some(Selection::Dir(p)) => format!("{p}/"),
            _ => String::new(),
        };
        let mut ok = 0;
        for p in &paths {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if let Ok(bytes) = std::fs::read(p)
                && self.container.add_file(&format!("{prefix}{name}"), &bytes).is_ok()
            {
                ok += 1;
            }
        }
        self.status = Some(format!("已添加 {ok}/{} 个文件", paths.len()));
    }

    fn add_folder_action(&mut self) {
        let Some(dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "imported".to_owned());
        let prefix = match &self.selected {
            Some(Selection::Dir(p)) => format!("{p}/{folder}"),
            _ => folder,
        };
        self.status = match self.container.add_dir(&dir, &prefix) {
            Ok(()) => Some(format!("已添加文件夹 → {prefix}/")),
            Err(e) => Some(format!("添加文件夹失败：{e}")),
        };
    }

    fn export_all_action(&mut self) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.status = match self.container.extract_all(&dir) {
                Ok(()) => Some(format!("已导出全部到 {}", dir.display())),
                Err(e) => Some(format!("导出失败：{e}")),
            };
        }
    }

    // ---- 右侧详情 ----

    fn detail_ui(&mut self, ui: &mut egui::Ui) {
        match self.selected.clone() {
            None => {
                ui.centered_and_justified(|ui| ui.weak("← 选择左侧的文件或目录"));
            }
            Some(Selection::Dir(prefix)) => self.dir_detail(ui, &prefix),
            Some(Selection::File(path)) => self.file_detail(ui, &path),
        }
        if let Some(s) = &self.status {
            ui.add_space(8.0);
            ui.small(s);
        }
    }

    fn dir_detail(&mut self, ui: &mut egui::Ui, prefix: &str) {
        ui.add_space(8.0);
        ui.heading(format!("📁 {prefix}"));
        ui.add_space(8.0);
        let pfx = format!("{prefix}/");
        let count = self.container.nodes().iter().filter(|n| n.path.starts_with(&pfx)).count();
        ui.label(format!("包含 {count} 个文件"));
        ui.add_space(12.0);
        if ui.button("📤 导出此目录…").clicked()
            && let Some(out) = rfd::FileDialog::new().pick_folder()
        {
            self.status = match self.container.extract_dir(prefix, &out) {
                Ok(()) => Some(format!("已导出目录到 {}", out.display())),
                Err(e) => Some(format!("导出失败：{e}")),
            };
        }
    }

    fn file_detail(&mut self, ui: &mut egui::Ui, path: &str) {
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

        // 操作按钮
        ui.horizontal(|ui| {
            if self.registry.find(&node).is_some() && ui.button("👁 应用内打开").clicked() {
                let ctx = ui.ctx().clone();
                match self.registry.find(&node).unwrap().open(&self.container, &node, &ctx) {
                    Ok(view) => {
                        self.current_view = Some((path.to_owned(), view));
                        self.status = None;
                    }
                    Err(e) => self.status = Some(format!("打开失败：{e}")),
                }
            }
            if ui.button("💾 导出…").clicked()
                && let Some(dest) = rfd::FileDialog::new().set_file_name(&file_name).save_file()
            {
                self.status = match self.container.extract_file(path, &dest) {
                    Ok(()) => Some(format!("已导出到 {}", dest.display())),
                    Err(e) => Some(format!("导出失败：{e}")),
                };
            }
            if ui.button("🗑 删除").clicked() {
                self.status = match self.container.remove_file(path) {
                    Ok(()) => {
                        self.selected = None;
                        self.current_view = None;
                        Some("已删除".to_owned())
                    }
                    Err(e) => Some(format!("删除失败：{e}")),
                };
            }
        });

        // 重命名 / 移动
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("重命名/移动为：");
            ui.add(
                egui::TextEdit::singleline(&mut self.rename_buf)
                    .hint_text(&node.path)
                    .desired_width(240.0),
            );
            if ui.button("确认").clicked() && !self.rename_buf.is_empty() {
                let to = self.rename_buf.clone();
                self.status = match self.container.rename_file(path, &to) {
                    Ok(()) => {
                        self.selected = Some(Selection::File(to.clone()));
                        self.rename_buf.clear();
                        Some(format!("已重命名为 {to}"))
                    }
                    Err(e) => Some(format!("重命名失败：{e}")),
                };
            }
        });

        // 活动视图（图片显示 / 播放器提示）
        if let Some((view_path, view)) = self.current_view.as_mut()
            && view_path == path
        {
            ui.separator();
            view.ui(ui);
        }
    }

    fn password_dialog(&mut self, ctx: &egui::Context) {
        if !self.pw_dialog {
            return;
        }
        let mut open = true;
        egui::Window::new("修改密码")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_pw).password(true).hint_text("新密码"));
                ui.add(egui::TextEdit::singleline(&mut self.new_pw2).password(true).hint_text("确认新密码"));
                ui.add_space(6.0);
                let matched = !self.new_pw.is_empty() && self.new_pw == self.new_pw2;
                if !matched && !self.new_pw2.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(230, 90, 90), "两次输入不一致");
                }
                if ui.add_enabled(matched, egui::Button::new("确认修改")).clicked() {
                    self.status = match self.container.change_password(self.new_pw.clone()) {
                        Ok(()) => Some("密码已修改".to_owned()),
                        Err(e) => Some(format!("改密码失败：{e}")),
                    };
                    self.new_pw.clear();
                    self.new_pw2.clear();
                    self.pw_dialog = false;
                }
            });
        if !open {
            self.pw_dialog = false;
            self.new_pw.clear();
            self.new_pw2.clear();
        }
    }
}

/// 递归渲染目录树：`current` 决定高亮，点击写入 `clicked`（不直接改选中项，
/// 由调用方统一应用，方便在切换时清理临时状态）。
fn show_tree(
    ui: &mut egui::Ui,
    node: &TreeNode,
    prefix: &str,
    current: &Option<Selection>,
    clicked: &mut Option<Selection>,
) {
    for (name, child) in &node.children {
        let full = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        match &child.file {
            // 文件
            Some(fs_node) => {
                let is_sel = matches!(current, Some(Selection::File(p)) if p == &fs_node.path);
                if ui.selectable_label(is_sel, format!("📄 {name}")).clicked() {
                    *clicked = Some(Selection::File(fs_node.path.clone()));
                }
            }
            // 目录：自定义折叠头（既能折叠，也能点选整个目录）
            None => {
                let is_sel = matches!(current, Some(Selection::Dir(p)) if p == &full);
                let id = ui.make_persistent_id(&full);
                let mut hit = false;
                egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true)
                    .show_header(ui, |ui| {
                        if ui.selectable_label(is_sel, format!("📁 {name}")).clicked() {
                            hit = true;
                        }
                    })
                    .body(|ui| show_tree(ui, child, &full, current, clicked));
                if hit {
                    *clicked = Some(Selection::Dir(full));
                }
            }
        }
    }
}

/// 全局样式：深色主题 + 更大字号 + 更松间距。
pub fn setup_style(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(14.0, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(12.5, FontFamily::Proportional)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
    });
}

/// 给 egui 装一个中文字体，否则中文显示为方块 □。
pub fn setup_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "/Library/Fonts/Arial Unicode.ttf",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
    ];

    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
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
