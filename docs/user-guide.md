# Veil 2.0 用户指南

## 快速开始

### 1. 创建容器

```bash
# 创建一个新的加密容器
veil init photos
```

这会在 `~/.veil/workspaces/default/photos/` 创建工作区。

**输出示例**：
```
创建新容器...
✓ 容器创建成功: photos
  工作区: /Users/mac/.veil/workspaces/default/photos
```

### 2. 添加文件

```bash
# 添加单个文件
veil add photos vacation.jpg

# 添加多个文件
veil add photos *.jpg
```

### 3. 列出文件

```bash
veil list photos
```

**输出示例**：
```
容器 'photos' 包含 3 个文件:

  1. vacation.jpg
     大小: 2.5 MB  加密时间: 2026-09-10 14:30:00
  2. family.jpg
     大小: 3.1 MB  加密时间: 2026-09-10 14:31:00
  3. sunset.jpg
     大小: 2.8 MB  加密时间: 2026-09-10 14:32:00

总大小: 8.4 MB
```

### 4. 提取文件

```bash
# 提取单个文件
veil ex photos vacation.jpg -o ~/Desktop/vacation.jpg

# 提取所有文件到目录
veil ex photos -o ~/Desktop/photos/
```

### 5. 删除文件

```bash
veil rm photos vacation.jpg
```

### 6. 打包容器

```bash
# 打包成 .veil 文件用于分享或备份
veil pack photos -o photos.veil
```

### 7. 解包容器

```bash
# 从 .veil 文件恢复到工作区
veil unpack photos.veil
```

## 高级用法

### 自定义工作区

```bash
# 创建自定义工作区
veil workspace add work ~/Documents/veil-work

# 在自定义工作区创建容器
veil init project-a --workspace work
```

### 专属工作区

```bash
# 创建专属工作区（容器独占）
veil init secrets --workspace ~/EncryptedVolume/veil --dedicated
```

### 容器迁移

```bash
# 将容器移动到不同工作区
veil move photos --to work
```

## 工作区架构说明

### 文件结构

```
~/.veil/
  ├── config.toml              ← 全局配置
  └── workspaces/
      └── default/             ← 默认工作区
          ├── photos/          ← 容器目录
          │   ├── .veil-meta   ← 加密的元数据
          │   ├── a3f2c1d4.enc ← 加密文件 1
          │   └── b7e4f9a8.enc ← 加密文件 2
          └── documents/       ← 另一个容器
```

### 两种文件格式

#### 工作区（本地使用）
- **位置**：`~/.veil/workspaces/default/photos/`
- **用途**：日常添加、删除、提取文件
- **特点**：支持并发操作，性能好

#### .veil 容器文件（分享/备份）
- **位置**：`photos.veil`
- **用途**：分享给他人、备份、跨设备传输
- **特点**：单个文件，自包含，可解包

### 区别说明

| 特性 | 工作区 | .veil 文件 |
|------|--------|-----------|
| 日常操作 | ✅ 快速 | ❌ 需解包 |
| 分享 | ❌ 不能 | ✅ 可以 |
| 并发 | ✅ 支持 | ❌ 不支持 |
| 备份 | ❌ 多文件 | ✅ 单文件 |

## 安全特性

### 加密算法
- **文件加密**：ChaCha20-Poly1305 (AEAD)
- **密钥派生**：Argon2id (64MB 内存，3 次迭代)
- **密钥长度**：256-bit

### 安全保证
- ✅ 每个容器独立密钥（独立 salt）
- ✅ 文件名完全加密（随机 ID）
- ✅ 元数据完全加密
- ✅ 认证加密（防篡改）
- ✅ 密钥从不持久化

### 密码建议
- 至少 12 个字符
- 包含大小写字母、数字、符号
- 不使用常见单词
- 不重复使用其他服务的密码

## 常见问题

### Q: 忘记密码怎么办？
A: 无法恢复。Veil 使用端到端加密，不存储主密钥。

### Q: 可以更改密码吗？
A: 可以使用 `veil passwd` 命令更改密码。

### Q: 工作区目录可以删除吗？
A: 不能！删除工作区目录会导致数据丢失。只能删除容器配置。

### Q: .veil 文件可以删除吗？
A: 可以。如果工作区还在，可以重新打包。

### Q: 如何备份？
A: 两种方式：
1. 打包成 .veil 文件备份
2. 直接备份工作区目录

### Q: 为什么操作很慢？
A: Argon2id 使用高安全参数（64MB 内存），这是设计选择。Release 模式会更快。

### Q: 支持多设备同步吗？
A: 通过 .veil 文件手动同步。未来版本会支持自动同步。

## 命令参考

### 容器管理
```bash
veil init <name>                    # 创建容器
veil list <name>                    # 列出文件
veil info <name>                    # 查看信息
veil passwd <name>                  # 更改密码
```

### 文件操作
```bash
veil add <name> <file>              # 添加文件
veil rm <name> <file>               # 删除文件
veil ex <name> <file> -o <output>  # 提取文件
veil mv <name> <from> <to>         # 重命名文件
```

### 打包/解包
```bash
veil pack <name> -o <output>       # 打包容器
veil unpack <file>                 # 解包容器
```

### 工作区管理
```bash
veil workspace list                # 列出工作区
veil workspace add <name> <path>   # 添加工作区
veil workspace rm <name>           # 删除工作区
```

### 实用工具
```bash
veil free <name>                   # 压缩容器
veil shell <name>                  # 交互式模式
```

## 性能提示

### Release 模式
始终使用 Release 模式编译：
```bash
cargo build --release
```

性能差异：
- Debug: ~26 秒/操作
- Release: ~2-3 秒/操作（预估）

### 批量操作
```bash
# 不推荐：多次调用
for file in *.jpg; do
    veil add photos $file
done

# 推荐：一次调用
veil add photos *.jpg
```

### 工作区位置
将工作区放在快速存储上（SSD），避免网络驱动器。

## 故障排除

### 密码错误
```
❌ 错误: 解密失败（密码错误或数据损坏）
```
**解决**：检查密码是否正确，确保容器未损坏。

### 容器已存在
```
❌ 错误: 容器 'photos' 已存在
```
**解决**：使用不同名称或删除现有容器。

### 权限错误
```
❌ 错误: Permission denied
```
**解决**：检查工作区目录权限。

### 配置错误
```
❌ 错误: 配置错误: 默认工作区未配置
```
**解决**：重新初始化配置 `rm ~/.veil/config.toml`。

## 最佳实践

### 1. 定期备份
```bash
# 每周打包备份
veil pack photos -o photos-$(date +%Y%m%d).veil
```

### 2. 使用强密码
```bash
# 使用密码管理器生成
# 示例：aB3$kL9@mN2#pQ7
```

### 3. 分类管理
```bash
# 按类别创建工作区
veil workspace add personal ~/Documents/veil-personal
veil workspace add work ~/Documents/veil-work

# 按类别创建容器
veil init family-photos --workspace personal
veil init project-docs --workspace work
```

### 4. 敏感数据隔离
```bash
# 使用专属工作区
veil init banking --workspace ~/EncryptedVolume/veil --dedicated
```

### 5. 环境变量
```bash
# 脚本中使用（谨慎）
export VEIL_PASSWORD="your-password"
veil add photos *.jpg
unset VEIL_PASSWORD
```

---

**版本**：2.0.0  
**更新日期**：2026-09-10  
**官方文档**：https://github.com/your-repo/veil
