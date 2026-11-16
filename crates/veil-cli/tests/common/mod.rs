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
    /// 创建一组测试私有 HOME 和工作目录，供多次 CLI 调用共享状态。
    /// 入参：password:&str 默认口令，允许空值用于失败测试，按 String 复制保存。
    /// 返回值：TestEnv，持有两个 TempDir 的清理责任和口令副本。
    /// 异常：临时目录创建失败会 unwrap panic。
    /// 注意事项：TempDir 析构尽力清理，不保证测试进程强制终止后无残留；口令是普通 String，未安全零化。
    pub fn new(password: &str) -> Self {
        Self {
            home: TempDir::new().unwrap(),
            work: TempDir::new().unwrap(),
            password: password.to_string(),
        }
    }

    /// 为当前测试生成使用默认口令的 CLI 命令对象。
    /// 入参：&self 为测试夹具，无其他参数。
    /// 返回值：assert_cmd::Command，已设置隔离环境和工作目录，尚未执行。
    /// 异常：无法定位测试构建的 veil 二进制时 panic。
    /// 注意事项：只构造命令，调用方继续设置参数并执行；结果状态不在此验证。
    pub fn command(&self) -> Command {
        self.command_with_password(&self.password)
    }

    /// 为成功或错误口令场景构造使用指定口令的 CLI 进程。
    /// 入参：&self；password:&str 本次 VEIL_PASSWORD，空串可用于测试必填校验。
    /// 返回值：Command，覆盖 HOME、VEIL_APP_DATA_DIR、VEIL_PASSWORD、VEIL_HINTS、VEIL_TEST_KDF 并设置 cwd。
    /// 异常：Command::cargo_bin 定位失败时 panic。
    /// 注意事项：继承其他未覆盖的环境变量，不是完全空白环境；fast KDF 仅适合测试，口令作为环境变量和进程输入不在此零化。
    pub fn command_with_password(&self, password: &str) -> Command {
        let mut command = Command::cargo_bin("veil").unwrap();
        command
            .env("HOME", self.home.path())
            .env("VEIL_APP_DATA_DIR", self.home.path().join("app"))
            .env("VEIL_PASSWORD", password)
            .env("VEIL_HINTS", "off")
            .env("VEIL_TEST_KDF", "fast")
            .current_dir(self.work.path());
        command
    }

    /// 通过真实 CLI 初始化一个容器，作为后续测试的数据前置。
    /// 入参：&self；veil_name:&str 容器名称，传给 CLI 由业务校验。
    /// 返回值：PathBuf，按原名称拼接的默认 .veil-link 预期路径。
    /// 异常：进程启动失败或退出非成功由 assert_cmd 使测试 panic/失败。
    /// 注意事项：同名容器可能生成不同链接名，本方法不解析命令输出确认实际路径；同时通过环境和位置参数提供相同默认口令。
    pub fn init(&self, veil_name: &str) -> PathBuf {
        self.command()
            .args(["init", veil_name, &self.password])
            .assert()
            .success();
        self.link_path(veil_name)
    }

    /// 计算测试工作目录下预期的默认链接位置。
    /// 入参：&self；veil_name:&str 直接用于格式化文件名的容器名称。
    /// 返回值：PathBuf，work.join(名称.veil-link)，不创建或检查文件。
    /// 异常：不返回错误。
    /// 注意事项：不规范化名称或过滤路径分隔符，绝对路径等输入可能逃离测试目录，只应使用可信测试名称。
    pub fn link_path(&self, veil_name: &str) -> PathBuf {
        self.work.path().join(format!("{}.veil-link", veil_name))
    }

    /// 为导入等场景准备 UTF-8 文本来源文件。
    /// 入参：&self；name:&str 相对文件名或路径；content:&str 完整文本内容。
    /// 返回值：PathBuf，已写入样本位置，父目录按需创建。
    /// 异常：父目录创建或写文件失败由 unwrap panic。
    /// 注意事项：覆盖已有目标，不原子写入；不校验路径边界，name 应为可信测试路径，返回文件由 work 的 TempDir 清理。
    pub fn write_file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.work.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    #[allow(dead_code)]
    /// 从测试 app.db 登记及链接 ID 解析容器密文目录。
    /// 入参：&self；link_path:&Path 现存链接位置。
    /// 返回值：PathBuf，配置中该 veil_id 的实际 veil_dir。
    /// 异常：应用配置加载、链接解析或 ID 解析失败时 unwrap panic。
    /// 注意事项：读取夹具 home/app/app.db，并非扫描任意工作区；配置加载可能执行初始化等副作用，不只做字符串拼接。
    pub fn veil_dir(&self, link_path: &Path) -> PathBuf {
        let mut config =
            veil_service::config::AppConfig::load_from(&self.home.path().join("app/app.db"))
                .unwrap();
        let link = veil_service::link::VeilLink::load(link_path).unwrap();
        config.resolve_veil(&link.veil.veil_id).unwrap().veil_dir
    }

    /// 将路径转成可传给 CLI 的拥有型字符串。
    /// 入参：&self（未使用）；path:impl `AsRef<Path>` 候选路径。
    /// 返回值：String，非 UTF-8 字节用替代字符的有损转换结果。
    /// 异常：不返回 Result，分配失败不保证恢复。
    /// 注意事项：不验证文件存在或路径合法性；有损转换可能无法精确指向原始非 UTF-8 路径。
    pub fn path(&self, path: impl AsRef<Path>) -> String {
        path.as_ref().to_string_lossy().into_owned()
    }
}
