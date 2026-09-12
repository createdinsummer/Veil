# Veil

Veil 是一个工作区模型的加密文件容器工具。CLI 通过 `.veil-link` 定位容器，工作区保存加密元数据和独立加密的文件内容，打包命令再把工作区整理为可传输的单个 `.veil` 文件。

## 文档导航

| 文档 | 适合读者 | 主要范围 |
| --- | --- | --- |
| [CLI 使用说明](crates/veil-cli/README.md) | 命令使用者、运维和脚本维护者 | 参数、工作流、配置、链接、打包解包和 CLI 限制 |
| [核心库说明](crates/veil-core/README.md) | 库维护者、格式维护者 | 存储模型、模块职责、磁盘格式、密钥流程、恢复和并发语义 |
| [密码强度测试工具](crates/veil-brute-force/README.md) | 授权测试者、安全评审者 | 支持范围、攻击模式、线程模型、历史记录和安全边界 |

## 产品定位

Veil 面向需要长期保管、迁移和备份本地文件的用户。它不依赖云端服务，容器身份由稳定的 `veil_id` 表示，展示名称和磁盘路径变化不会改变容器身份。

项目包含三个主要部分：

| 组件 | 职责 |
| --- | --- |
| `veil-core` | 工作区操作、单文件容器、配置、链接、元数据、密钥派生和内容加解密 |
| `veil-cli` | 用户命令、密码输入、链接恢复、工作区打包和解包 |
| `veil-brute-force` | 对自行创建的单文件容器进行密码强度测试 |

## 工作模型

初始化容器后，Veil 会生成两个相互关联但用途不同的对象。

| 对象 | 内容 | 说明 |
| --- | --- | --- |
| `.veil-link` | 容器 ID、展示名称、卷 ID 和工作区相对路径 | 只是入口和定位信息，不保存文件内容 |
| 工作区目录 | `.veil-meta` 和随机命名的 `.enc` 文件 | 真正的数据存储位置 |

默认工作区位于 `~/.veil/workspaces/default`，每个容器使用独立目录。全局配置位于 `~/.veil/config.toml`，会缓存容器、卷和链接信息。链接被删除时，如果配置中仍有原始字节副本，Veil 会尝试恢复。

`veil pack` 会把工作区整理为一个便于传输的 `.veil` 文件。该文件包含加密元数据和已有密文，不会在打包时重新加密文件内容。`veil unpack` 使用原容器密码验证并还原工作区，然后生成新的链接和容器记录。

## 命令入口

当前 CLI 提供容器创建、文件管理、信息查看、密码修改、交互式 shell、打包解包、配置和链接维护等命令。完整的参数和操作说明集中在 [CLI 文档](crates/veil-cli/README.md)。

| 操作类别 | 命令 |
| --- | --- |
| 创建 | `veil init` |
| 文件管理 | `veil add`、`veil rm`、`veil mv`、`veil ex` |
| 查看 | `veil list`、`veil free`、`veil info`、`veil exists` |
| 会话与维护 | `veil shell`、`veil passwd`、`veil config`、`veil link` |
| 迁移 | `veil pack`、`veil unpack` |
| 帮助 | `veil help` |

命令同时支持位置参数和选项参数。生产环境建议使用交互式密码输入或环境变量，避免把密码直接留在 shell 历史和进程列表中。

## 安全模型

工作区容器使用由用户密码派生的主密钥保护元数据和文件内容。密钥派生采用 Argon2id，内容加密使用 ChaCha20-Poly1305，并为每个文件保存独立 nonce。工作区密码修改会重新加密元数据和全部文件。

核心库同时提供单文件容器模型。该模型使用 x25519 和 age 加密文件 blob 与目录索引，使用 Argon2id 和 ChaCha20-Poly1305 保护容器私钥。单文件格式、打包格式和恢复语义详见 [Core 文档](crates/veil-core/README.md)。

工作区文件读取依赖认证加密识别密文损坏；单文件容器还会通过 BLAKE3 校验完整读取后的明文内容。

## 构建与测试

开发环境需要 Rust 工具链和 Cargo。构建 CLI 使用 `cargo build --package veil-cli`，发布构建使用 `cargo build --release --package veil-cli`。

运行完整测试使用 `cargo test --workspace --all-targets`。生成库文档使用 `cargo doc --workspace --no-deps`。

## 关键边界

- CLI 的 `ex` 当前只处理单文件；`add` 支持文件、目录递归导入和目标路径。
- 同一工作区没有跨进程写锁。
- `passwd` 会重新加密工作区中的全部文件。
- `.veil-link` 依赖稳定的卷身份和工作区相对路径。
- 忘记密码后没有恢复通道。

详细限制分别记录在 CLI 和 Core 文档中。

## 项目结构

| 路径 | 说明 |
| --- | --- |
| `crates/veil-core` | 核心库和存储格式 |
| `crates/veil-cli` | `veil` 命令行程序和集成测试 |
| `crates/veil-brute-force` | 密码强度测试工具 |

## 许可证

项目按 Apache License 2.0 发布，具体条款见仓库根目录的 `LICENSE` 文件。
