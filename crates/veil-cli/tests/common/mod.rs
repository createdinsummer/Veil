use assert_cmd::Command;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct TestEnv {
    pub home: TempDir,
    pub work: TempDir,
    pub password: String,
}

impl TestEnv {
    pub fn new(password: &str) -> Self {
        Self {
            home: TempDir::new().unwrap(),
            work: TempDir::new().unwrap(),
            password: password.to_string(),
        }
    }

    pub fn command(&self) -> Command {
        self.command_with_password(&self.password)
    }

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

    pub fn init(&self, container_name: &str) -> PathBuf {
        self.command()
            .args(["init", container_name, &self.password])
            .assert()
            .success();
        self.link_path(container_name)
    }

    pub fn link_path(&self, container_name: &str) -> PathBuf {
        self.work
            .path()
            .join(format!("{}.veil-link", container_name))
    }

    pub fn write_file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.work.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    pub fn path(&self, path: impl AsRef<Path>) -> String {
        path.as_ref().to_string_lossy().into_owned()
    }
}
