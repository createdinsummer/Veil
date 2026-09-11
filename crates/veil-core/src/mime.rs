//! 按文件扩展名猜测 MIME 类型。
//!
//! 结果用于填充 [`crate::index::FileMeta::mime`]，供上层按内容类型选择处理方式。
//!
//! 此处只做静态扩展名映射，不读取文件内容，因此不会产生 I/O 开销，也无法识别
//! 缺少扩展名或扩展名与实际内容不符的文件；未知类型返回 `None`。

use std::path::Path;

/// 按扩展名猜测 MIME 类型，扩展名比较不区分大小写。
///
/// # 参数
/// - `path`：文件路径或文件名；只有最后一个扩展名参与判断。
///
/// # 返回
/// 识别成功时返回标准 MIME 字符串；无扩展名、扩展名非 UTF-8 或未知时返回 `None`。
pub fn guess_mime(path: &str) -> Option<String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();

    let mime = match ext.as_str() {
        // 图片格式用于上层图片查看器分发。
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        // 视频格式保留常见容器扩展名到标准 MIME 的映射。
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        // 音频格式用于系统播放器或音频组件分发。
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        // 文档类型只做基础识别，不尝试解析文件内容。
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" => "text/markdown",
        _ => return None,
    };
    Some(mime.to_owned())
}

/// MIME 扩展名映射的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证常见扩展名的大小写和多级路径处理。
    #[test]
    fn guesses_common_types() {
        // 大小写不敏感、能从多级路径取扩展名
        assert_eq!(guess_mime("a.JPG").as_deref(), Some("image/jpeg"));
        assert_eq!(guess_mime("photos/2024/clip.mp4").as_deref(), Some("video/mp4"));
        assert_eq!(guess_mime("song.flac").as_deref(), Some("audio/flac"));
        // 无扩展名 / 未知扩展名 → None
        assert_eq!(guess_mime("README"), None);
        assert_eq!(guess_mime("weird.xyz"), None);
    }
}
