//! `veil rm` 子命令：删除工作区容器中的文件。
//! 所属模块：CLI内部路径删除。核心run定位、取得密码、调用remove_path并区分类型提示；依赖服务删除、密码交互和本地化，目标是删除目录树权威引用并尽力回收密文。
//!
//! 业务流程：解析容器并验证密码后，服务层先提交更新后的元数据，再清理失去引用的
//! 密文文件；删除目录会递归删除其下全部文件。容器根目录和 `..` 逃逸会被拒绝。

use crate::error::Result;
use colored::Colorize;
use veil_service::veil_ops::RemovedPathKind;

/// 删除容器内部文件或目录子树，并按对象类型显示结果。
/// 入参：veil_name为选择器；file_name（&str）为内部目标路径；password为可选显式密码，None回退环境/TTY。
/// 返回值：逻辑删除成功后Ok(())，区分文件/目录提示。
/// 异常：定位、输入、源不存在、租约/数据库/删除释放错误转CommandError。
/// 注意事项：无额外确认提示；同步物理删除是尽力行为，失败可能留孤儿密文；后半释放失败也可能已经删掉引用。
///
/// # 示例
/// ```bash
/// veil rm photos vacation.jpg
/// ```
pub fn run(veil_name: &str, file_name: &str, password: Option<String>) -> Result<()> {
    // 先解析稳定 ID，确保后续删除针对的是最终工作区而不是输入本身。
    let resolved = super::resolve_veil(veil_name)?;
    let manager = super::container_client(&resolved);

    // 删除前先输出目标；真正删除仍需要密码和元数据查找成功。
    crate::outln!(
        "{}",
        crate::i18n::t1("rm.deleting", "path", file_name).yellow()
    );
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // core 会先提交更新后的元数据，再清理失去引用的密文文件。
    let removed = manager.remove_path(file_name, password)?;

    // 删除结果区分文件与目录，选用不同的成功文案。
    let message_key = match removed {
        RemovedPathKind::File => "rm.deleted",
        RemovedPathKind::Directory => "rm.deleted_directory",
    };
    crate::outln!(
        "{}",
        crate::i18n::t1(message_key, "path", file_name).green()
    );

    Ok(())
}
