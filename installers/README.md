# macFUSE 离线安装包

## 📦 目录说明

这个目录用于存放 macFUSE 的离线安装包，供网络受限的用户使用。

## 📥 下载安装包

在有网络的环境中运行：

```bash
cd ..
./download_macfuse.sh
```

这会自动下载最新的 macFUSE 安装包到此目录。

## 📋 预期文件

下载完成后，此目录应包含：

```
installers/
├── README.md              # 本文件
└── macfuse-4.7.1.dmg      # macFUSE 安装包 (~4.2 MB)
```

## 🚀 使用方法

将整个项目（包括此目录）分发给用户后，用户运行：

```bash
./setup_fuse_offline.sh
```

脚本会自动使用此目录中的安装包进行离线安装。

## 🔐 安全验证

下载完成后，可以验证文件完整性：

```bash
# 查看文件大小（应该约 4.2 MB）
ls -lh macfuse-*.dmg

# 测试挂载（确保文件未损坏）
hdiutil verify macfuse-*.dmg
```

## 📚 更多信息

详细的离线安装指南请查看：`../OFFLINE_INSTALL_GUIDE.md`
