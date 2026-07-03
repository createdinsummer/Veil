//! # mime —— 按文件扩展名猜测 MIME 类型
//!
//! 给 [`crate::index::FsNode::mime`] 赋值，供查看器（GUI）按类型分发：
//! `image/*` 用图片查看器、`video/*`/`audio/*` 用播放器等。
//!
//! 这是**纯扩展名映射**，不读文件内容；覆盖常见媒体/文档类型，未知返回 `None`
//! （GUI 据此走「释放到本地由用户处理」的兜底）。将来要更准可换成读魔数嗅探。

use std::path::Path;

/// 按扩展名猜测 MIME 类型（扩展名大小写不敏感）。未知返回 `None`。
///
/// # 参数
/// - `path`: 文件路径或文件名（只用其扩展名）
///
/// # 返回
/// - `Some(mime)`：识别到的 MIME 类型，如 `"image/jpeg"`
/// - `None`：无扩展名、扩展名非法或未知类型
pub fn guess_mime(path: &str) -> Option<String> {
    // 取扩展名并转小写；没有扩展名 → None（`?` 提前返回）
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();

    let mime = match ext.as_str() {
        // 图片
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        // 视频
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        // 音频
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        // 文档
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" => "text/markdown",
        // 未知扩展名
        _ => return None,
    };
    Some(mime.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

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
