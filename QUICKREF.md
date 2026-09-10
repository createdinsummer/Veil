# Veil 2.0 快速参考

## ✅ 已迁移命令

| 命令 | 用法 | 说明 |
|------|------|------|
| **init** | `veil init <name>` | 创建新容器 |
| **add** | `veil add <container> <file>` | 添加文件 |
| **rm** | `veil rm <container> <file>` | 删除文件 |
| **ex** | `veil ex <container> <file> -o <output>` | 提取文件 |
| **info** | `veil info <container>` | 查看信息 |

## 🔄 工作区架构 vs 旧格式

### 旧格式（1.x）
```bash
veil init photos.veil          # 创建单文件
veil add photos.veil pic.jpg   # 添加到文件
veil info photos.veil          # 查看文件
```
- ❌ 不支持并发
- ❌ 修改需要重写整个文件
- ❌ 删除留下死空间

### 新格式（2.0）
```bash
veil init photos               # 创建工作区
veil add photos pic.jpg        # 独立加密
veil info photos               # 查看工作区
```
- ✅ 支持并发操作
- ✅ 增量更新
- ✅ 完整删除

## 📂 目录结构

```
~/.veil/
  ├── config.toml                    # 全局配置
  └── workspaces/
      └── default/                   # 默认工作区
          └── photos/                # 容器目录
              ├── .veil-meta         # 加密元数据
              ├── a3f2c1d4.enc       # 加密文件
              └── b7e4f9a8.enc       # 加密文件
```

## 🔐 安全特性

- **密钥派生**：Argon2id (64MB 内存, 3 次迭代)
- **文件加密**：ChaCha20-Poly1305 (AEAD)
- **文件名加密**：随机 ID (16 字符十六进制)
- **独立密钥**：每个容器独立 salt
- **独立 nonce**：每个文件独立 nonce

## 🚀 快速开始

```bash
# 1. 创建容器
veil init myfiles

# 2. 添加文件
veil add myfiles document.pdf
veil add myfiles photo.jpg

# 3. 查看内容
veil info myfiles

# 4. 提取文件
veil ex myfiles document.pdf -o ~/Desktop/document.pdf

# 5. 删除文件
veil rm myfiles photo.jpg
```

## ⚠️ 注意事项

1. **不兼容旧格式**：旧的 `.veil` 文件需要迁移工具转换
2. **性能考虑**：每次操作需要密钥派生（约 2-3 秒）
3. **密码管理**：使用强密码，忘记无法恢复

## 📝 环境变量

```bash
# 避免交互式输入密码（谨慎使用）
export VEIL_PASSWORD="your-password"
veil add photos *.jpg
unset VEIL_PASSWORD
```

## 🔜 即将推出

- [ ] `veil pack` - 打包成 .veil 文件
- [ ] `veil unpack` - 解包 .veil 文件
- [ ] `veil mv` - 重命名文件
- [ ] `veil passwd` - 更改密码
- [ ] `veil workspace` - 工作区管理

## 📚 文档

- [用户指南](docs/user-guide.md)
- [技术实现](docs/technical-implementation.md)
- [开发进度](docs/development-progress.md)
- [迁移报告](docs/migration-report.md)

---

**版本**：2.0.0-dev  
**状态**：核心功能完成
