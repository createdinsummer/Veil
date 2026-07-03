//! # viewer —— 应用内查看器（适配器模式）
//!
//! 见 `doc/viewer-design.md`。核心两个 trait：
//! - [`Viewer`]（适配器）：判断能否处理某文件、并打开它；
//! - [`ActiveView`]（活动视图）：打开后每帧渲染，**Drop 时释放资源**（纹理 / 临时明文）。
//!
//! [`ViewerRegistry`] 按 `FileMeta.mime` 找第一个能处理的查看器；找不到 → 由 UI 走
//! 「导出到本地」兜底。渲染层只调 `veil-core` 的数据能力，不碰加解密逻辑。

use anyhow::Result;
use eframe::egui;
use veil_core::container::Container;
use veil_core::index::FileMeta;
use veil_core::temp::TempPlaintext;

/// 打开后的活动视图：每帧渲染；`Box` 被 Drop 时其持有的资源自动释放。
pub trait ActiveView {
    fn ui(&mut self, ui: &mut egui::Ui);
}

/// 一个查看器（适配器）：能否处理 + 如何打开。
pub trait Viewer {
    /// 能否处理这个文件（按 MIME 判断）。
    fn can_handle(&self, meta: &FileMeta) -> bool;
    /// 打开它，返回活动视图。`path` 是容器内虚拟路径。
    fn open(
        &self,
        container: &Container,
        path: &str,
        meta: &FileMeta,
        ctx: &egui::Context,
    ) -> Result<Box<dyn ActiveView>>;
}

/// 查看器注册表：按顺序找第一个能处理的。新增类型只需再注册一个 [`Viewer`]。
pub struct ViewerRegistry {
    viewers: Vec<Box<dyn Viewer>>,
}

impl ViewerRegistry {
    pub fn with_defaults() -> Self {
        Self { viewers: vec![Box::new(ImageViewer), Box::new(MediaViewer)] }
    }

    /// 找一个能处理该文件的查看器（找不到 → None）。
    pub fn find(&self, meta: &FileMeta) -> Option<&dyn Viewer> {
        self.viewers.iter().map(|v| v.as_ref()).find(|v| v.can_handle(meta))
    }
}

// ───────── 图片查看器：解密到内存 → egui 纹理（明文不落盘）─────────

struct ImageViewer;

struct ImageView {
    texture: egui::TextureHandle,
}

impl Viewer for ImageViewer {
    fn can_handle(&self, meta: &FileMeta) -> bool {
        meta.mime.as_deref().is_some_and(|m| m.starts_with("image/"))
    }

    fn open(
        &self,
        container: &Container,
        path: &str,
        _meta: &FileMeta,
        ctx: &egui::Context,
    ) -> Result<Box<dyn ActiveView>> {
        let bytes = container.read_file(path)?; // 解密到内存（不落盘）
        let img = image::load_from_memory(&bytes)?;
        let rgba = img.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
        let texture = ctx.load_texture(path.to_owned(), color, egui::TextureOptions::LINEAR);
        Ok(Box::new(ImageView { texture }))
    }
}

impl ActiveView for ImageView {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::both().show(ui, |ui| {
            ui.add(
                egui::Image::new(&self.texture)
                    .max_width(ui.available_width())
                    .maintain_aspect_ratio(true),
            );
        });
    }
}

// ───────── 媒体查看器：解密到临时文件 → 系统播放器（V1）─────────

struct MediaViewer;

struct MediaView {
    _temp: TempPlaintext, // RAII 守卫：视图存活期间临时明文在，Drop 即删
}

impl Viewer for MediaViewer {
    fn can_handle(&self, meta: &FileMeta) -> bool {
        meta.mime
            .as_deref()
            .is_some_and(|m| m.starts_with("video/") || m.starts_with("audio/"))
    }

    fn open(
        &self,
        container: &Container,
        path: &str,
        _meta: &FileMeta,
        _ctx: &egui::Context,
    ) -> Result<Box<dyn ActiveView>> {
        let temp = container.extract_to_temp(path)?; // 解密到受控临时位置（优先 RAM）
        open::that(temp.path())?; // 唤起系统默认播放器
        Ok(Box::new(MediaView { _temp: temp }))
    }
}

impl ActiveView for MediaView {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("▶ 已在系统播放器中打开。");
        ui.add_space(6.0);
        ui.weak("关闭此视图（选择其他文件或锁定）将删除临时明文。");
    }
}
