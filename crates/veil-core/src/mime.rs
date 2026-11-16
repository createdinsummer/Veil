//! 按文件扩展名猜测 MIME 类型。
//!
//! 结果供上层统计和展示 Veil 内文件的 MIME 类型分布。
//!
//! 此处只做静态扩展名映射，不读取文件内容，因此不会产生 I/O 开销，也无法识别
//! 缺少扩展名或扩展名与实际内容不符的文件；未知类型返回 `None`。
//!
//! # 业务定位
//!
//! 容器内的文件以密文存储在磁盘上，无法靠读取内容识别类型；本模块基于用户
//! 可见的文件名扩展名给出 MIME 类型（MIME=互联网媒体类型标识，如 image/png），
//! 供上层查询和统计使用，不参与加密逻辑。调用方若用于展示分发，仍需自行判断
//! 是否支持对应格式，不能把扩展名当作文件内容的可靠证明。

//!
//! 核心函数：guess_mime 根据 Path 提取最后一个扩展名，经 ASCII 小写化后查静态映射表。
//! 依赖：标准库路径拆解与字符串处理；没有内容解密、媒体解析或外部系统调用，目标是提供低成本展示线索。
use std::path::Path;

/// 按用户可见文件名的最后扩展名提供低成本媒体类型展示线索。
/// 入参：path（&str）为有效 UTF-8 路径或名称，按当前平台 Path 规则提取扩展名，不要求文件存在。
/// 返回值：`Option<String>`，表内扩展名（ASCII 大小写不敏感）返回 MIME，缺扩展名或未知类型返回 None。
/// 异常：不返回 I/O 或格式错误；字符串分配失败不保证恢复。
/// 注意事项：不读内容或解密，扩展名可被随意修改，不能用于安全判断或保证预览器支持；输入本来是 &str，接口不接受原始非 UTF-8 路径。
pub fn guess_mime(path: &str) -> Option<String> {
    // 取最后扩展名并做 ASCII 小写化；无扩展名时提前 None。to_str 分支保留通用路径接口形式，输入已由 &str 保证 UTF-8。
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();

    // 静态映射表：只覆盖常见类型，未知扩展名一律返回 None（不猜测）。
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
        // 不在表内的扩展名：不猜，交由调用方按未知类型处理。
        _ => return None,
    };
    // 映射表是 'static 字符串，返回时拷贝成拥有所有权的 String。
    Some(mime.to_owned())
}

/// MIME 扩展名映射的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证常见扩展名的大小写和多级路径处理。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn guesses_common_types() {
        // 大小写不敏感、能从多级路径取扩展名
        assert_eq!(guess_mime("a.JPG").as_deref(), Some("image/jpeg"));
        assert_eq!(
            guess_mime("photos/2024/clip.mp4").as_deref(),
            Some("video/mp4")
        );
        assert_eq!(guess_mime("song.flac").as_deref(), Some("audio/flac"));
        // 无扩展名 / 未知扩展名 → None
        assert_eq!(guess_mime("README"), None);
        assert_eq!(guess_mime("weird.xyz"), None);
    }
}
