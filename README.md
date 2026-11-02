# Veil

Veil 是一个 Work 模型的加密文件工具。CLI 通过 `.veil-link` 定位 Veil，`veil_dir` 保存加密元数据和独立加密的文件内容，打包命令再把 Veil 整理为可传输的单个 `.veil` 文件。

## 文档导航

| 文档 | 适合读者 | 主要范围 |
| --- | --- | --- |
| [CLI 使用说明](crates/veil-cli/README.md) | 命令使用者、运维和脚本维护者 | 参数、工作流、配置、链接、打包解包和 CLI 限制 |
| [核心库说明](crates/veil-core/README.md) | 库维护者、格式维护者 | 存储模型、模块职责、磁盘格式、密钥流程、恢复和并发语义 |
| [SQLite 存储改造设计](docs/sqlite-storage-design.md) | 后续开发者 | 目标表结构、SQLCipher、文件级并发、双密钥轮值及开发验收清单；已接入新存储及恢复测试 |
| [密码强度测试工具](crates/veil-brute-force/README.md) | 授权测试者、安全评审者 | 支持范围、攻击模式、线程模型、历史记录和安全边界 |
| [录屏转长截图](crates/veil-scrollshot/README.md) | 独立工具使用者、库集成开发者 | 垂直滚动录屏拼接、固定栏识别和回滚去重 |

## 产品定位

Veil 面向需要长期保管、迁移和备份本地文件的用户。它不依赖云端服务，Veil 身份由稳定的 `veil_id` 表示，展示名称和磁盘路径变化不会改变身份。

项目包含以下主要部分：

| 组件 | 职责 |
| --- | --- |
| `veil-core` | 加密、数据库连接、版本比较、执行身份和打包格式等底层能力 |
| `veil-service` | 容器操作、工作区、链接及锁和版本控制的业务编排 |
| `veil-cli` | 用户命令、密码输入、链接恢复、Veil 打包和解包 |
| `veil-brute-force` | 对自行创建并通过 `veil pack` 生成的 `.veil` 文件进行密码强度测试 |
| `veil-app` | Tauri 桌面原型，提供容器创建、打开、锁定和文件列表 |

## 工作模型

初始化 Veil 后，会生成两个相互关联但用途不同的对象。

| 对象 | 内容 | 说明 |
| --- | --- | --- |
| `.veil-link` | 容器 ID、展示名称、容器创建时间和链接创建时间 | 只是入口和定位信息，不保存文件内容 |
| Work 目录 `work_root` | 以 `veil_id` 命名的容器目录 | 仅作为存放容器的目录 |
| Veil 目录 `veil_dir` | SQLCipher `veil.db` 和随机命名的 `.enc` 文件 | 真正的数据存储位置 |

默认空间位于各平台应用数据目录的 `Veil/spaces/<space_id>`，本机索引和设置位于 `Veil/app.db`。桌面端与 CLI 使用一致的目录定位策略，移动端注入应用私有目录。空间具有稳定 ID，实际路径由应用数据库登记。容器身份、目录树、内容版本和文件密钥由各容器的 `veil.db` 管理。

`.veil-link` 正文仍使用 JSONC，本机数据库缓存完整结构化正文以恢复缺失链接。未解锁时通过本机登记展示容器；解锁后核验实际身份并刷新缓存。没有缓存时，可以用目录和密码重新登记。

`veil pack` 导出加密数据库一致快照及引用的密文，`veil unpack` 验证密码、身份和文件认证后在新设备独立还原。

## 命令入口

当前 CLI 提供 Veil 创建、文件管理、信息查看、密码修改、交互式 shell、打包解包、配置和链接维护等命令。完整的参数和操作说明集中在 [CLI 文档](crates/veil-cli/README.md)。

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

容器数据库使用固定 SQLCipher 4 参数和原生密码派生，整库加密。独立文件使用随机文件数据密钥与分块 ChaCha20-Poly1305。普通改密只修改数据库密码保护；`veil passwd --full` 以两把文件密钥轮换并支持中断恢复。

业务并发采用数据库中的文件级租约，提交同时检查基础版本与持锁凭证。打包格式及一致性边界见 [Core 文档](crates/veil-core/README.md)。

旧 JSONC 配置、旧 `.veil-info` 容器和第 1 版打包文件均不读取，也不自动迁移。

## 构建与测试

开发环境需要 Rust 工具链和 Cargo。构建 CLI 使用 `cargo build --package veil-cli`，发布构建使用 `cargo build --release --package veil-cli`。

运行完整测试使用 `cargo test --workspace --all-targets`；故障恢复测试使用 `cargo test -p veil-service --features fault-injection --test storage_fault_tests`。生成库文档使用 `cargo doc --workspace --no-deps`。

桌面端使用 React、TypeScript 和 Tauri。在 `apps/veil-app` 执行 `npm ci`、`npm run build` 或 `npm run tauri dev`。打开容器时选择包含 `veil.db` 的目录；空间、最近记录和偏好均通过 service 使用本机数据库。支持添加、预览、导出和删除文件。

## 关键边界

- CLI 的 `ex` 当前只处理单文件；`add` 支持文件、目录递归导入和目标路径。
- `passwd` 默认只修改密码保护层；`passwd --full` 才会重新加密 Veil 中的全部文件。
- `.veil-link` 仅按容器 ID 查找已登记工作区，移动容器后无需修改链接。
- 忘记密码后没有恢复通道。

详细限制分别记录在 CLI 和 Core 文档中。

## 项目结构

| 路径 | 说明 |
| --- | --- |
| `crates/veil-core` | 核心能力和存储格式 |
| `crates/veil-service` | 容器业务服务 |
| `crates/veil-cli` | `veil` 命令行程序和集成测试 |
| `crates/veil-brute-force` | 密码强度测试工具 |
| `crates/veil-scrollshot` | 独立的录屏转长截图模块与命令行程序 |
| `apps/veil-app` | Tauri 和 React 桌面原型，独立于根 Cargo 工作空间 |

## 许可证

项目按 Apache License 2.0 发布，具体条款见仓库根目录的 `LICENSE` 文件。
