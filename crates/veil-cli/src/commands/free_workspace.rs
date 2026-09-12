//! `veil free` 子命令：以简化树状视图展示容器内容。

use crate::error::Result;
use colored::Colorize;
use std::collections::BTreeMap;

/// `free` 展示用的文件系统节点。
enum TreeNode {
    /// 文件及其明文大小。
    File(u64),
    /// 目录及其按名称排序的子节点。
    Directory(BTreeMap<String, TreeNode>),
}

/// 按原始相对路径排序并缩进展示文件，同时输出文件和总大小统计。
///
/// # 错误
/// 容器解析、密码读取或元数据解密失败时返回错误。
pub fn run_workspace(container_name: &str, password: Option<String>) -> Result<()> {
    // 解析后绑定稳定 ID，展示名称以实际元数据为准。
    let resolved = super::resolve_container(container_name)?;
    let manager = super::workspace_manager(&resolved);

    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 解密清单后即可完整展示树状内容，无需读取文件密文本体。
    let metadata = manager.read_meta(password)?;

    crate::outln!("\n{}", crate::i18n::t("free.title").cyan().bold());

    if metadata.files.is_empty() {
        crate::outln!(
            "{}",
            format!("  {}", crate::i18n::t("common.empty")).bright_black()
        );
    } else {
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

/// 从扁平文件清单构造真正的目录树。
fn build_tree(files: &[veil_core::metadata::FileEntry]) -> BTreeMap<String, TreeNode> {
    let mut root = BTreeMap::new();

    for file in files {
        let parts: Vec<_> = file
            .original_name
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let Some((name, directories)) = parts.split_last() else {
            continue;
        };

        let mut current = &mut root;
        for directory in directories {
            let entry = current
                .entry((*directory).to_string())
                .or_insert_with(|| TreeNode::Directory(BTreeMap::new()));
            if !matches!(entry, TreeNode::Directory(_)) {
                *entry = TreeNode::Directory(BTreeMap::new());
            }
            let TreeNode::Directory(children) = entry else {
                unreachable!();
            };
            current = children;
        }

        current.insert((*name).to_string(), TreeNode::File(file.size));
    }

    root
}

/// 按标准树状连接线递归渲染节点。
fn render_tree(nodes: &BTreeMap<String, TreeNode>, prefix: &str) {
    let count = nodes.len();
    for (index, (name, node)) in nodes.iter().enumerate() {
        let is_last = index + 1 == count;
        let connector = if is_last { "└── " } else { "├── " };
        match node {
            TreeNode::File(size) => {
                crate::outln!("{prefix}{connector}{name} ({})", format_size(*size));
            }
            TreeNode::Directory(children) => {
                crate::outln!("{prefix}{connector}{name}/");
                let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
                render_tree(children, &child_prefix);
            }
        }
    }
}

/// 将字节数格式化为 B、KB、MB 或 GB，并保留两位小数。
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

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
