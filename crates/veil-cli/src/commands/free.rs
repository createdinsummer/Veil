//! `veil free` 子命令：以简化树状视图展示容器内容。
//! 所属模块：CLI命令适配。TreeNode为展示树，run/build_tree/render_tree构建和渲染，format_size显示大小；依赖元数据与BTreeMap，目标是路径层级展示。
//!
//! 按每个文件的原始相对路径构造真实父子目录树，用 `├──`/`└──`/`│` 连接线绘制；
//! 文件与目录按名称排序，目录名带 `/`，最后输出文件数量与总大小统计。
//! 只解密元数据清单，不读取任何文件密文本体。

use crate::error::Result;
use colored::Colorize;
use std::collections::BTreeMap;

/// `free` 展示用的文件系统节点。
///
/// 从扁平文件清单（每个文件一条完整原始路径）构造目录树：
/// 文件节点保存明文大小；目录节点保存按名称排序的子节点映射，
/// BTreeMap 自动按键名排序，因此渲染天然稳定。
enum TreeNode {
    /// 文件及其明文大小。
    File(u64),
    /// 目录及其按名称排序的子节点。
    Directory(BTreeMap<String, TreeNode>),
}

/// 读取元数据并显示由文件路径推导的树状视图与大小统计。
/// 入参：veil_name（&str）为容器选择器；password（`Option<String>`）为显式密码，None回退环境/TTY。
/// 返回值：完成显示后Ok(())，无文件输出空状态。
/// 异常：定位/密码/数据库读取错误返回CommandError；极端总大小u64求和溢出可能panic或按构建溢出行为处理。
/// 注意事项：只使用metadata.files，不读取directories，空目录不会显示；这不是磁盘目录树。
pub fn run(veil_name: &str, password: Option<String>) -> Result<()> {
    // 解析后绑定稳定 ID，展示名称以实际元数据为准。
    let resolved = super::resolve_veil(veil_name)?;
    let manager = super::container_client(&resolved);

    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 解密清单后即可完整展示树状内容，无需读取文件密文本体。
    let metadata = manager.read_meta(password)?;

    crate::outln!("\n{}", crate::i18n::t("free.title").cyan().bold());

    // 空容器只显示空状态行，不渲染目录树。
    if metadata.files.is_empty() {
        crate::outln!(
            "{}",
            format!("  {}", crate::i18n::t("common.empty")).bright_black()
        );
    } else {
        // 有内容时：先把扁平清单变成目录树，再递归渲染连接线。
        let tree = build_tree(&metadata.files);
        render_tree(&tree, "");
    }

    // 统计信息直接基于元数据中的明文大小，不会触发额外解密。
    let file_count = metadata.files.len();
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();

    crate::outln!("\n{}", crate::i18n::t("free.stats_title").cyan());
    crate::outln!(
        "{}",
        crate::i18n::t1("free.file_count", "count", &file_count.to_string())
    );
    crate::outln!(
        "{}",
        crate::i18n::t2(
            "free.total_size",
            "bytes",
            &total_size.to_string(),
            "mb",
            &format!("{:.2}", total_size as f64 / 1_048_576.0)
        )
    );

    Ok(())
}

/// 从每份文件的内部路径构造展示目录树。
/// 入参：files（`&[FileEntry]`）每项为完整逻辑文件路径和大小。
/// 返回值：`BTreeMap<String,TreeNode>`，键为本层名称，值为文件大小或子目录映射，按键排序。
/// 异常：无Result返回；内部类型分支不变量破坏可触发unreachable panic。
/// 注意事项：忽略空路径分段；中间同名文件会被目录替换，末级insert也会覆盖先前节点，异常清单不报错。不会从目录元数据补空目录。
fn build_tree(files: &[veil_core::metadata::FileEntry]) -> BTreeMap<String, TreeNode> {
    // 根映射：容器根下的顶层名称 → 节点。
    let mut root = BTreeMap::new();

    // 遍历每个文件条目，把一条完整路径逐级挂到树中。
    for file in files {
        // 按 `/` 切出路径分量，忽略空分量（如路径首尾的斜杠）。
        let parts: Vec<_> = file
            .original_name
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        // 最后一个分量是文件名，其余全是目录链；空路径（异常数据）跳过。
        let Some((name, directories)) = parts.split_last() else {
            continue;
        };

        // 沿目录链逐级进入（或创建）目录节点。
        let mut current = &mut root;
        for directory in directories {
            let entry = current
                .entry((*directory).to_string())
                .or_insert_with(|| TreeNode::Directory(BTreeMap::new()));
            // 中间分量若已被同名文件占用（清单异常），以目录为准重建，
            // 保证后代文件仍能挂入树中。
            if !matches!(entry, TreeNode::Directory(_)) {
                *entry = TreeNode::Directory(BTreeMap::new());
            }
            // 取出目录节点的子映射继续向下，此处必然是目录。
            let TreeNode::Directory(children) = entry else {
                unreachable!();
            };
            current = children;
        }

        // 文件名分量落位为叶子文件节点，保存明文大小。
        current.insert((*name).to_string(), TreeNode::File(file.size));
    }

    root
}

/// 递归输出一层有序节点及其后代的树状连接线。
/// 入参：nodes（&`BTreeMap<String,TreeNode>`）为当前层名称/节点映射；prefix（&str）为上层累积缩进和竖线。
/// 返回值：无，文件行显示大小，目录行带斜杠并递归。
/// 异常：无Result返回；极深递归受调用栈限制。
/// 注意事项：每轮一条同级节点，末项用└──，其余用├──；仅渲染现有映射，不查库或文件系统。
fn render_tree(nodes: &BTreeMap<String, TreeNode>, prefix: &str) {
    let count = nodes.len();
    // 遍历当前层全部节点（BTreeMap 保证名称有序）。
    for (index, (name, node)) in nodes.iter().enumerate() {
        // 同级最后一个节点用 └── 收尾，其余用 ├──。
        let is_last = index + 1 == count;
        let connector = if is_last { "└── " } else { "├── " };
        match node {
            TreeNode::File(size) => {
                // 文件行：连接线 + 名称 + 括号内格式化大小。
                crate::outln!("{prefix}{connector}{name} ({})", format_size(*size));
            }
            TreeNode::Directory(children) => {
                // 目录行：连接线 + 名称 + `/` 后缀。
                crate::outln!("{prefix}{connector}{name}/");
                // 子层前缀：同级还有后续节点时保留 │ 竖线，否则用空格保持对齐。
                let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
                render_tree(children, &child_prefix);
            }
        }
    }
}

/// 将字节数换算成便于终端阅读的容量文本。
/// 入参：bytes（u64）为文件或包逻辑字节数。
/// 返回值：String：小于1024用整数B，其余按1024次幂显示KB/MB/GB并保留两位小数。
/// 异常：无Result返回。
/// 注意事项：标签使用KB但计算为二进制1024；超过GB仍用GB，f64转换在极大整数时可能舍入。
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    // 先匹配达到的最大单位阈值，再以1024次幂换算；不改变原始统计值。
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
