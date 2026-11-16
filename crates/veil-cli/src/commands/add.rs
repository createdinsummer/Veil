//! `veil add` 子命令：接收输入并调用文件导入服务。
//! 所属模块：CLI文件导入。ConflictPolicy保存用户策略；decide_conflict处理一条审核；run编排流式导入与结果展示。依赖服务导入管线、TTY和密码输入，目标是在冲突可控的情况下提交内容。
//!
//! 业务流程：解析容器并验证密码后，通过流式导入把本地文件或目录递归加密写入
//! veil_dir，同时更新 veil.db 的目录树。同名目标采用替换语义；冲突处理策略由
//! `--on-conflict` 指定或交互询问。

use crate::error::Result;
use colored::Colorize;
use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};
use veil_service::commands::{ImportConflict, ImportDecision};

/// `--on-conflict` 的同名目标处理策略。
///
/// 导入时遇到容器内已存在的同名目标，按策略决定覆盖、跳过或报错。
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ConflictPolicy {
    /// 逐个询问用户（默认值）。
    Ask,
    /// 直接报错终止导入。
    Error,
    /// 跳过同名目标，继续导入其余文件。
    Skip,
    /// 覆盖同名目标。
    Overwrite,
}

/// 为一条导入或导出冲突取得本次和后续处理策略。
/// 入参：conflict（&ImportConflict）含target、same_content、is_directory和error；policy（&mut ConflictPolicy）为当前策略，可被交互的a/k改成后续覆盖/跳过。
/// 返回值：返回ImportDecision::Overwrite/Skip/Error；Error是交给调用层处理的决定。
/// 异常：Ask下非终端为WorkError，EOF或q为Cancelled；stdin/stderr I/O错误传播。
/// 注意事项：自动策略直接返回，只有Ask才限制目录/带错误项不能覆盖；最终是否允许覆盖由流水线再检查。输入只接受小写o/s/a/k/q，无效输入重复询问。
pub(super) fn decide_conflict(
    conflict: &ImportConflict,
    policy: &mut ConflictPolicy,
) -> veil_core::error::Result<ImportDecision> {
    use veil_core::error::VeilError;
    // 非询问策略直接落定，不需要终端交互。
    match policy {
        ConflictPolicy::Error => return Ok(ImportDecision::Error),
        ConflictPolicy::Skip => return Ok(ImportDecision::Skip),
        ConflictPolicy::Overwrite => return Ok(ImportDecision::Overwrite),
        ConflictPolicy::Ask => {}
    }
    // 只有 Ask 策略才需要终端；脚本/管道环境无法询问，直接报错避免流程永久阻塞。
    if !io::stdin().is_terminal() {
        return Err(VeilError::WorkError(format!(
            "目标路径已存在：{}；非交互环境请指定 --on-conflict error|skip|overwrite",
            conflict.target
        )));
    }
    // 循环直到用户给出有效选择。
    loop {
        // 目录不能覆盖；存在底层错误时同样不能覆盖，只能跳过或取消。
        let can_overwrite = !conflict.is_directory && conflict.error.is_none();
        let detail = if let Some(error) = conflict.error.as_deref() {
            // 优先展示底层错误文本，解释为什么该目标不能覆盖。
            error
        } else if conflict.is_directory {
            "目标是目录，不支持覆盖"
        } else if conflict.same_content {
            "内容相同"
        } else {
            "内容不同"
        };
        // 输出冲突详情与可选操作，提示写到 stderr 以保持 stdout 输出干净。
        eprintln!("\n目标已存在：{}（{}）", conflict.target, detail);
        eprint!("选择 [o]覆盖/[s]跳过/[a]后续全部覆盖/[k]后续全部跳过/[q]取消：");
        io::stderr().flush()?;
        let mut input = String::new();
        // 输入流关闭（EOF）按用户取消处理，不陷入死循环。
        if io::stdin().read_line(&mut input)? == 0 {
            return Err(VeilError::Cancelled);
        }
        match input.trim() {
            // 覆盖当前文件（仅文件且无底层错误时允许）。
            "o" if can_overwrite => return Ok(ImportDecision::Overwrite),
            // 跳过当前文件，其余文件继续导入。
            "s" => return Ok(ImportDecision::Skip),
            // 本次导入后续全部覆盖：改写策略，避免每个冲突都询问。
            "a" if can_overwrite => {
                *policy = ConflictPolicy::Overwrite;
                return Ok(ImportDecision::Overwrite);
            }
            // 本次导入后续全部跳过。
            "k" => {
                *policy = ConflictPolicy::Skip;
                return Ok(ImportDecision::Skip);
            }
            // 取消整个导入操作。
            "q" => return Err(VeilError::Cancelled),
            // 无效输入（或当前目标不能覆盖）时提示重新选择。
            _ => eprintln!("请输入有效选项；该目标可能只能跳过或取消。"),
        }
    }
}

/// 按单来源映射将本地文件或目录流式加密导入，展示实际写入和跳过数量。
/// 入参：veil_name为目标选择器；source（&str）为本地文件/目录；dest（`Option<&str>`）为显式内部目标映射；password为可选显式密码；conflict_policy为同名策略。
/// 返回值：全部处理或跳过后Ok(())，无发现文件时仅提示空目录。
/// 异常：定位/密码、来源/路径/冲突、取消、租约/加密/提交或流水线错误转CommandError。
/// 注意事项：prompt只取得密码，真正验证在服务流水线。完成数包含跳过，写入数需相减；分批提交，失败可已有文件成功，不提供全导入回滚。
pub fn run(
    veil_name: &str,
    source: &str,
    dest: Option<&str>,
    password: Option<String>,
    mut conflict_policy: ConflictPolicy,
) -> Result<()> {
    // 先解析容器，保证导入目标绑定稳定 ID 而不是链接文本。
    let container = super::resolve_veil(veil_name)?;
    crate::outln!("{}", crate::i18n::t("opening_veil").cyan());
    // 此处只取得密码文本；真正开库验证在下面服务流水线开始阶段完成。
    let password = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;
    use secrecy::ExposeSecret;
    // 流式导入：每个同名冲突都按当前策略决定覆盖/跳过/取消或询问用户。
    let result = container
        .stream_import(
            Path::new(source),
            dest,
            password.expose_secret(),
            |conflict| decide_conflict(conflict, &mut conflict_policy),
            super::transfer::progress,
        )
        .map_err(super::map_service_error)?;
    // 总文件数为 0 表示源是空目录，只提示不报错。
    if result.total_files == 0 {
        crate::outln!("{}", crate::i18n::t("add.empty_dir").yellow());
        return Ok(());
    }
    // 跳过的文件单独汇报，避免与成功数混淆。
    if result.skipped_files > 0 {
        crate::outln!("已跳过 {} 个文件", result.skipped_files);
    }
    // 实际导入数 = 完成数 - 跳过数。
    let imported = result.completed_files - result.skipped_files;
    // 全部被跳过时不打印成功信息。
    if imported == 0 {
        return Ok(());
    }
    if let Some(target) = result.target {
        // 单文件导入：显示容器内最终路径。
        crate::outln!(
            "{}",
            crate::i18n::t1("add.file_added", "path", &target).green()
        );
        if let Some(name) = result.encrypted_name {
            // 附带显示磁盘上的随机加密文件名，便于用户对照。
            crate::outln!(
                "{}",
                crate::i18n::t1("common.encrypted_name", "name", &name).bright_black()
            );
        }
    } else {
        // 多文件（目录）导入：显示成功数量。
        crate::outln!(
            "{}",
            crate::i18n::t1("add.files_added", "count", &imported.to_string()).green()
        );
    }
    Ok(())
}
