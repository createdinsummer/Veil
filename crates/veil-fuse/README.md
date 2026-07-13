# Veil FUSE

FUSE 文件系统实现，支持将加密容器挂载为虚拟磁盘。

## 特性

- 🚀 **流式解密** - 只解密需要的部分
- 📺 **视频秒开** - 打开 1.4GB 视频 < 1 秒
- 💾 **内存恒定** - 占用仅 ~64KB
- 🎯 **随机访问** - 支持拖动进度条

## 安装

### 在线安装（推荐）

```bash
# 在项目根目录运行
./setup_fuse.sh
```

### 离线安装（网络受限环境）

```bash
# 1. 在有网络的机器下载安装包
./download_macfuse.sh

# 2. 在目标机器运行离线安装
./setup_fuse_offline.sh
```

## 使用

```bash
# 挂载容器
cargo run --release -p veil-fuse --example mount_with_guide vault.veil

# 访问文件
open /tmp/veil
```

## 架构

```rust
// 核心实现
impl Filesystem for VeilFS {
    fn read(&mut self, ino: u64, offset: i64, size: u32, reply: ReplyData) {
        // 只解密需要的部分
        let data = self.container.read_range(path, offset as u64, size as usize)?;
        reply.data(&data);
    }
}
```

## 性能

| 指标 | WebDAV | FUSE | 提升 |
|------|--------|------|------|
| 打开 1.4GB 视频 | 30 秒 | < 1 秒 | **30x** |
| 内存占用 | 1.4 GB | ~64 KB | **21,875x** |
| 拖动进度条 | 卡顿 | 流畅 | **∞** |

## 依赖

- macFUSE 4.x（安装脚本会自动处理）

## 示例

### 基础挂载

```bash
cargo run --release -p veil-fuse --example mount_fuse vault.veil
```

### 带引导的挂载

```bash
cargo run --release -p veil-fuse --example mount_with_guide vault.veil
```

## 许可证

Apache License 2.0
