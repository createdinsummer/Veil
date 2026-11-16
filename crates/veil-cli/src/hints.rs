//! 一次性操作提示的判定与渲染。
//!
//! 提示级别可由 `VEIL_HINTS` 临时覆盖，否则读取全局配置。首次创建、打包和解包提示
//! 在真正输出前先登记“已展示”标记；链接恢复是可重复条件提示，不记录次数。
//!
//! 核心清单：`current_level`/`set_hint_level` 读写提示级别；`should_show` 询问服务层
//! 是否展示；四个 `show_*` 函数渲染首次创建/首次打包/首次解包/链接恢复提示；
//! `print_hint_box` 输出统一边框提示框。

use crate::error::Result;
use crate::i18n;
use colored::Colorize;
use std::path::Path;
use veil_service::config::HintLevel;
use veil_service::hints::HintKind;
use veil_service::{ServiceRequest, ServiceResponse};

/// 取得环境覆盖与持久偏好合并后的提示级别。
/// 入参：无，读取VEIL_HINTS。
/// 返回值：HintLevel：合法覆盖优先，否则服务读设置；服务失败/协议不符回退Full。
/// 异常：不返回错误，环境不可读视为无覆盖。
/// 注意事项：每次新建临时服务查询，可能访问应用库；非法覆盖值不是强制Off。
pub fn current_level() -> HintLevel {
    // 进程级覆盖变量存在与否都交给服务层，这里只负责取值。
    let value = std::env::var("VEIL_HINTS").ok();
    match crate::commands::request(ServiceRequest::EffectiveHintLevel {
        override_value: value.as_deref(),
    }) {
        Ok(ServiceResponse::EffectiveHintLevel(level)) => level,
        // 服务层异常时保守地按完整提示处理，避免丢失重要使用说明。
        _ => HintLevel::Full,
    }
}

/// 请求保存用户的提示偏好。
/// 入参：level（&str）为full/brief/off文本。
/// 返回值：成功返回服务规范化后的HintLevel。
/// 异常：非法级别、设置保存或非预期响应返回CommandError。
/// 注意事项：不修改VEIL_HINTS，所以新设置未必立即成为本进程有效级别。
pub fn set_hint_level(level: &str) -> Result<HintLevel> {
    match crate::commands::request(ServiceRequest::SetHintLevel { level })? {
        ServiceResponse::HintLevelSet(level) => Ok(level),
        // 请求与响应类型不匹配属于服务协议错误。
        _ => Err(veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into()),
    }
}

/// 请求服务决定某类提示是否应显示，并对一次性类型先登记标记。
/// 入参：kind（HintKind）为首次创建/备份/还原或链接恢复类别。
/// 返回值：ShowHint的bool；服务失败或响应不符为false。
/// 异常：无错误返回，底层失败不阻止主业务。
/// 注意事项：一次性标记发生在真正打印前，输出失败也可能已记展示；链接恢复不记次数。
fn should_show(kind: HintKind) -> bool {
    let value = std::env::var("VEIL_HINTS").ok();
    match crate::commands::request(ServiceRequest::ShouldShowHint {
        kind,
        override_value: value.as_deref(),
    }) {
        Ok(ServiceResponse::ShowHint(show)) => show,
        _ => false,
    }
}

/// 展示首次创建后的数据目录、链接和备份用法。
/// 入参：veil_name为名称兜底；link_path（&Path）为实际链接；veil_dir为加密目录。
/// 返回值：无；服务允许显示才输出，否则直接返回。
/// 异常：无Result；输出宏失败会结束进程。
/// 注意事项：优先链接文件名，非UTF-8/缺文件名才用名称拼链接；不验证路径仍存在。一次性类型会在打印前记录标记。
pub fn show_first_init_hint(veil_name: &str, link_path: &Path, veil_dir: &Path) {
    if !should_show(HintKind::FirstInit) {
        return;
    }

    // 优先使用实际链接文件名，异常路径才回退到容器名。
    let link_name = link_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}.veil-link", veil_name));

    crate::outln!();
    // 第一部分：标题，优先用实际生成的链接文件名，容器名只作兜底。
    crate::outln!("{}", i18n::t1("hints.init.title", "name", &link_name));
    crate::outln!();
    // 第二部分：真正存放加密数据的目录（veil_dir）位置。
    crate::outln!("  {}", i18n::t("hints.init.storage_line"));
    crate::outln!("    {}", veil_dir.display());
    crate::outln!();
    // 第三部分：如何把整个容器打包为单个 .veil 文件用于备份迁移。
    crate::outln!("  {}", i18n::t("hints.init.pack_title"));
    crate::outln!(
        "    {}",
        i18n::t1("hints.init.pack_command", "veil", &link_name)
    );
    crate::outln!();
    // 第四部分：如何在另一台设备上用打包文件还原容器。
    crate::outln!("  {}", i18n::t("hints.init.unpack_title"));
    crate::outln!("    {}", i18n::t("hints.init.unpack_command"));
    crate::outln!();
    // 第五部分：退出/关闭 shell 的操作提示。
    crate::outln!("  {}", i18n::t("hints.init.close_title"));
    crate::outln!("    {}", i18n::t("hints.init.close_command"));
    crate::outln!();
}

/// 展示首次备份中链接入口与加密包的区别。
/// 入参：veil_name为链接名兜底；link_path（`Option<&Path>`）为可选已知链接；output_path（&str）为包显示路径；size（u64）为包字节数。
/// 返回值：无；服务允许显示才输出，否则直接返回。
/// 异常：无Result；输出宏失败会结束进程。
/// 注意事项：按1024²换算并标MB；不读取包或确认链接内容。一次性类型会在打印前记录标记。
pub fn show_pack_explain_hint(
    veil_name: &str,
    link_path: Option<&Path>,
    output_path: &str,
    size: u64,
) {
    if !should_show(HintKind::FirstPack) {
        return;
    }

    // 打包可能从链接路径或容器名触发，两种来源都要正确展示。
    let link_display = link_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("{}.veil-link", veil_name));
    // 字节数换算为 MB 展示，便于用户直观感知包大小。
    let size_mb = format!("{:.2}", size as f64 / 1_048_576.0);

    print_hint_box(
        i18n::t("hints.pack.title"),
        &[
            // 第一行：本次打包对应的链接文件路径。
            i18n::t1("hints.pack.link_line", "path", &link_display),
            // 第二行：解释链接只负责定位，不包含文件内容。
            i18n::t("hints.pack.link_desc").to_string(),
            // 空行分隔链接说明与包文件说明。
            String::new(),
            // 第四行：包文件路径与大小。
            i18n::t2(
                "hints.pack.output_line",
                "path",
                output_path,
                "size_mb",
                &size_mb,
            ),
            // 第五行：解释 .veil 包包含加密数据库快照与密文。
            i18n::t("hints.pack.output_desc").to_string(),
        ],
    );
}

/// 展示首次还原后的链接与文件数量说明。
/// 入参：veil_name为还原展示名；link_path为生成链接；file_count（usize）为还原文件数。
/// 返回值：无；服务允许显示才输出，否则直接返回。
/// 异常：无Result；输出宏失败会结束进程。
/// 注意事项：标题中的包名用展示名加.veil推导，不是实际来源包路径。一次性类型会在打印前记录标记。
pub fn show_unpack_explain_hint(veil_name: &str, link_path: &Path, file_count: usize) {
    if !should_show(HintKind::FirstUnpack) {
        return;
    }

    // 解包提示重点说明新链接与包文件的对应关系。
    let link_name = link_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}.veil-link", veil_name));

    // 标题同时展示包文件与生成的链接文件名；正文说明文件数量与容器名。
    print_hint_box(
        &i18n::t2(
            "hints.unpack.title",
            "package",
            &format!("{}.veil", veil_name),
            "link",
            &link_name,
        ),
        &[i18n::t3(
            "hints.unpack.body",
            "link",
            &link_name,
            "count",
            &file_count.to_string(),
            "name",
            veil_name,
        )],
    );
}

/// 提示客户端本次定位采用链接缓存恢复分支。
/// 入参：link_path（&Path）为显示路径。
/// 返回值：无；服务允许显示才输出，否则直接返回。
/// 异常：无Result；输出宏失败会结束进程。
/// 注意事项：条件提示可重复，不验证本次是否真正创建文件。一次性类型会在打印前记录标记。
pub fn show_link_recovery_hint(link_path: &Path) {
    if !should_show(HintKind::LinkRecovery) {
        return;
    }

    print_hint_box(
        i18n::t("hints.link_recovery.title"),
        &[i18n::t1(
            "hints.link_recovery.body",
            "path",
            &link_path.display().to_string(),
        )],
    );
}

/// 输出支持文件类型帮助主题。
/// 入参：无。
/// 返回值：无，打印对应本地化正文。
/// 异常：无Result，输出失败由宏结束进程。
/// 注意事项：不检查提示级别，也不写一次性标记。
pub fn show_files_help() {
    crate::outln!("\n{}", i18n::t("help.files.content"));
}

/// 以统一边框输出已渲染的多行帮助。
/// 入参：title（&str）为标题；lines（&[String]）每项一行正文，可为空行。
/// 返回值：无，按顺序输出各行及关闭提示说明。
/// 异常：无Result，stdout写失败由宏结束进程。
/// 注意事项：不自动本地化正文、不测终端宽度/换行，不单独决定是否应显示；调用方先完成提示级别判定。
pub fn print_hint_box(title: &str, lines: &[String]) {
    crate::outln!();
    // 顶部边框线，灰暗色弱化视觉干扰。
    crate::outln!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_black());
    // 提示框标题，青色加粗突出主题。
    crate::outln!("{}", title.cyan().bold());
    crate::outln!();
    // 逐行输出已渲染的正文（调用方负责顺序与本地化）。
    for line in lines {
        crate::outln!("{}", line);
    }
    // 底部边框线与顶部对称。
    crate::outln!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_black());
    crate::outln!();
    // 末行告知用户如何关闭这类一次性提示。
    crate::outln!("{}", i18n::t("hints.dismiss").bright_black());
    crate::outln!();
}
