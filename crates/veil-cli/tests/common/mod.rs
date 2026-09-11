//! CLI 集成测试共享环境。
//!
//! 为每个测试提供隔离的 HOME、工作目录和密码，并统一构造关闭提示、启用快速 KDF
//! 的 `veil` 命令，减少各测试文件中的环境搭建重复。

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// 隔离的 CLI 测试环境。
pub struct TestEnv {
    /// 作为 HOME 使用的临时目录。
    pub home: TempDir,
    /// 作为命令当前工作目录使用的临时目录。
    pub work: TempDir,
    /// 默认测试密码。
    pub password: String,
}

impl TestEnv {
    /// 创建独立 HOME 和工作目录，并记录默认密码。
    pub fn new(password: &str) -> Self {
        Self {
            home: TempDir::new().unwrap(),
            work: TempDir::new().unwrap(),
            password: password.to_string(),
        }
    }

    /// 使用默认密码构造 `veil` 命令。
    pub fn command(&self) -> Command {
        self.command_with_password(&self.password)
    }

    /// 使用指定密码并注入隔离测试环境变量构造 `veil` 命令。
    pub fn command_with_password(&self, password: &str) -> Command {
        let mut command = Command::cargo_bin("veil").unwrap();
        command
            .env("HOME", self.home.path())
            .env("VEIL_PASSWORD", password)
            .env("VEIL_HINTS", "off")
            .env("VEIL_TEST_KDF", "fast")
            .current_dir(self.work.path());
        command
    }

    /// 初始化容器并返回默认链接文件路径。
    pub fn init(&self, container_name: &str) -> PathBuf {
        self.command()
            .args(["init", container_name, &self.password])
            .assert()
            .success();
        self.link_path(container_name)
    }

    /// 返回当前工作目录下指定容器名称的默认链接路径。
    pub fn link_path(&self, container_name: &str) -> PathBuf {
        self.work
            .path()
            .join(format!("{}.veil-link", container_name))
    }

    /// 在工作目录下创建文本文件，必要时先创建父目录。
    pub fn write_file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.work.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    /// 将路径转换为适合传给命令行参数的字符串。
    pub fn path(&self, path: impl AsRef<Path>) -> String {
        path.as_ref().to_string_lossy().into_owned()
    }
}
