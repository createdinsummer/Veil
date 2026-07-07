# Veil CLI 环境配置指南

## 安装到系统路径

### 方法 1：使用安装脚本（推荐）

编译后运行安装脚本：

```bash
# 1. 编译项目
cargo build --release --package veil-cli

# 2. 运行打包脚本
./build.sh

# 3. 安装
cd release
./install.sh
```

### 方法 2：手动安装

#### macOS / Linux

```bash
# 编译
cargo build --release --package veil-cli

# 复制到系统路径（需要 sudo）
sudo cp target/release/veil /usr/local/bin/

# 或复制到用户目录（不需要 sudo）
mkdir -p ~/.local/bin
cp target/release/veil ~/.local/bin/

# 验证
veil --version
```

#### Windows

```powershell
# 编译
cargo build --release --package veil-cli

# 复制到用户目录
mkdir -Force $env:USERPROFILE\bin
copy target\release\veil.exe $env:USERPROFILE\bin\

# 添加到 PATH（PowerShell）
$env:Path += ";$env:USERPROFILE\bin"
[Environment]::SetEnvironmentVariable("Path", $env:Path, [EnvironmentVariableScope]::User)

# 验证
veil --version
```

## 配置 PATH 环境变量

### Bash (~/.bashrc)

```bash
# 添加到文件末尾
export PATH="$PATH:$HOME/.local/bin"
```

应用配置：
```bash
source ~/.bashrc
```

### Zsh (~/.zshrc)

```bash
# 添加到文件末尾
export PATH="$PATH:$HOME/.local/bin"
```

应用配置：
```bash
source ~/.zshrc
```

### Fish (~/.config/fish/config.fish)

```fish
# 添加到文件末尾
set -gx PATH $PATH $HOME/.local/bin
```

应用配置：
```bash
source ~/.config/fish/config.fish
```

### Windows

#### 方法 1：系统设置（GUI）

1. 右键"此电脑" → "属性"
2. "高级系统设置" → "环境变量"
3. 在"用户变量"中找到 `Path`
4. 点击"编辑" → "新建"
5. 添加 `%USERPROFILE%\bin`
6. 确定保存

#### 方法 2：PowerShell

```powershell
# 永久添加到用户 PATH
$newPath = "$env:USERPROFILE\bin"
$currentPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableScope]::User)
[Environment]::SetEnvironmentVariable("Path", "$currentPath;$newPath", [EnvironmentVariableScope]::User)
```

## 验证安装

```bash
# 检查版本
veil --version
# 输出: veil 0.1.0

# 查看帮助
veil --help

# 测试功能
veil init test.veil testpass
veil free test.veil testpass
rm test.veil
```

## 安装位置推荐

### 系统级安装（所有用户可用）

**推荐位置**: `/usr/local/bin` (macOS/Linux)

```bash
sudo cp target/release/veil /usr/local/bin/
```

优点：
- ✅ 默认在 PATH 中
- ✅ 所有用户都能使用
- ✅ 系统标准位置

缺点：
- ⚠️ 需要 sudo 权限
- ⚠️ 可能与系统包管理器冲突

### 用户级安装（推荐）

**推荐位置**: `~/.local/bin` (Linux/macOS) 或 `%USERPROFILE%\bin` (Windows)

```bash
mkdir -p ~/.local/bin
cp target/release/veil ~/.local/bin/
```

优点：
- ✅ 不需要 sudo 权限
- ✅ 不影响其他用户
- ✅ 易于管理和删除

缺点：
- ⚠️ 需要手动添加到 PATH

### 开发安装（Cargo）

使用 `cargo install`:

```bash
cargo install --path crates/veil-cli
```

自动安装到: `~/.cargo/bin/veil`

优点：
- ✅ 自动添加到 PATH（如果配置了 cargo）
- ✅ 易于更新
- ✅ 开发者标准方式

## Shell 补全（可选）

### Bash

```bash
# 生成补全脚本
veil --completion bash > ~/.local/share/bash-completion/completions/veil

# 或手动加载
veil --completion bash >> ~/.bashrc
```

### Zsh

```bash
# 生成补全脚本
veil --completion zsh > ~/.zsh/completions/_veil

# 添加到 .zshrc
fpath=(~/.zsh/completions $fpath)
autoload -U compinit && compinit
```

### Fish

```bash
# 生成补全脚本
veil --completion fish > ~/.config/fish/completions/veil.fish
```

注意：Shell 补全功能需要在代码中实现，当前版本暂未包含。

## 卸载

### 使用卸载脚本

```bash
cd release
./uninstall.sh
```

### 手动卸载

```bash
# 查找安装位置
which veil

# 删除文件
rm /usr/local/bin/veil
# 或
rm ~/.local/bin/veil
# 或
rm ~/.cargo/bin/veil
```

## 常见问题

### Q: 执行 veil 提示"command not found"

A: 检查以下几点：
1. 确认 veil 已复制到正确位置：`ls -la ~/.local/bin/veil`
2. 确认有执行权限：`chmod +x ~/.local/bin/veil`
3. 确认路径在 PATH 中：`echo $PATH | grep .local/bin`
4. 重新加载 shell 配置：`source ~/.bashrc`

### Q: macOS 提示"无法验证开发者"

A: 运行以下命令移除隔离属性：
```bash
xattr -d com.apple.quarantine /usr/local/bin/veil
```

### Q: 如何更新 veil

A: 重新编译并复制：
```bash
cd /path/to/Veil
git pull
cargo build --release --package veil-cli
sudo cp target/release/veil /usr/local/bin/
```

### Q: 如何安装到自定义路径

A: 
```bash
# 复制到自定义路径
cp target/release/veil /path/to/custom/dir/

# 添加到 PATH
export PATH="$PATH:/path/to/custom/dir"
```

## 多版本管理

如果需要同时保留多个版本：

```bash
# 重命名为带版本号的名称
cp target/release/veil /usr/local/bin/veil-0.1.0

# 创建符号链接
ln -sf /usr/local/bin/veil-0.1.0 /usr/local/bin/veil

# 切换版本
ln -sf /usr/local/bin/veil-0.2.0 /usr/local/bin/veil
```

## 环境变量

Veil CLI 支持以下环境变量：

```bash
# 默认密码（用于脚本，不推荐生产环境）
export VEIL_PASSWORD="your-password"

# 使用
veil free vault.veil  # 自动使用 VEIL_PASSWORD

# 新密码（用于 passwd 命令）
export VEIL_NEW_PASSWORD="new-password"
```

## 系统集成示例

### Cron 定时备份

```bash
# 编辑 crontab
crontab -e

# 添加定时任务（每天凌晨 2 点备份）
0 2 * * * VEIL_PASSWORD="pass" /usr/local/bin/veil add backup.veil ~/Documents/ >> /var/log/veil-backup.log 2>&1
```

### Systemd 服务（Linux）

创建 `/etc/systemd/system/veil-backup.service`:

```ini
[Unit]
Description=Veil Backup Service
After=network.target

[Service]
Type=oneshot
Environment="VEIL_PASSWORD=your-password"
ExecStart=/usr/local/bin/veil add /backup/vault.veil /data/
User=backup

[Install]
WantedBy=multi-user.target
```

启用服务：
```bash
sudo systemctl enable veil-backup.service
sudo systemctl start veil-backup.service
```

## 开发环境配置

如果你是开发者，推荐使用以下配置：

```bash
# 1. 使用 cargo alias
alias veil-dev='cargo run --package veil-cli --'

# 2. 使用
veil-dev init test.veil

# 3. 或创建符号链接
ln -sf $(pwd)/target/debug/veil ~/.local/bin/veil-dev
```

这样可以同时保留开发版和稳定版。
