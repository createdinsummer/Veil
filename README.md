# Veil

Veil 是一个 Work 模型的加密文件工具。CLI 通过 `.veil-link` 定位 Veil，`veil_dir` 保存加密元数据和独立加密的文件内容，打包命令再把 Veil 整理为可传输的单个 `.veil` 文件。

## 文档导航

| 文档 | 适合读者 | 主要范围 |
| --- | --- | --- |
| [CLI 使用说明](crates/veil-cli/README.md) | 命令使用者、运维和脚本维护者 | 参数、工作流、配置、链接、打包解包和 CLI 限制 |
| [核心库说明](crates/veil-core/README.md) | 库维护者、格式维护者 | 存储模型、模块职责、磁盘格式、密钥流程、恢复和并发语义 |
| [密码强度测试工具](crates/veil-brute-force/README.md) | 授权测试者、安全评审者 | 支持范围、攻击模式、线程模型、历史记录和安全边界 |
| [录屏转长截图](crates/veil-scrollshot/README.md) | 独立工具使用者、库集成开发者 | 垂直滚动录屏拼接、固定栏识别和回滚去重 |

## 产品定位

Veil 面向需要长期保管、迁移和备份本地文件的用户。它不依赖云端服务，Veil 身份由稳定的 `veil_id` 表示，展示名称和磁盘路径变化不会改变身份。

项目包含以下主要部分：

| 组件 | 职责 |
| --- | --- |
| `veil-core` | 加密、存储、文件锁、版本比较和打包格式等底层能力 |
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
| Veil 目录 `veil_dir` | `.veil-info` 和随机命名的 `.enc` 文件 | 真正的数据存储位置 |

默认 `work_root` 位于 `~/.veil/works/default`，每个 Veil 使用独立 `veil_dir`。本机配置位于 `~/.veil/config.jsonc`，移动盘配置位于 `<盘根目录>/.veil/config.jsonc`；配置登记工作区，并缓存容器信息和链接正文。相对工作区路径基于所属配置目录解析，默认值为 `works/default`。程序遍历已登记工作区，以 `.veil-info` 中的容器 ID 核验身份；容器整体移动到已登记工作区后，原链接继续有效。同 ID 的多个副本报告歧义，目标盘未挂载时报未找到。配置文件与 `.veil-link` 均使用 JSONC，支持注释和尾逗号，更新时保留已有字段的注释。链接被删除时，如果配置中仍有对应的结构化正文，Veil 会尝试重新生成链接。

`veil pack` 会把 Veil 整理为一个便于传输的 `.veil` 文件。该文件包含加密元数据和已有密文，不会在打包时重新加密文件内容。`veil unpack` 使用原 Veil 密码验证并还原目录，然后生成新的链接和 Veil 记录。

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

Work 模型中的 Veil 使用 Argon2id 从用户密码和 salt 派生密码保护密钥，元数据中保存随机数据主密钥。文件内容使用数据主密钥进行分块流式认证加密，元数据使用密码保护密钥加密。默认修改密码只重新保护元数据；`veil passwd --full` 会轮换数据主密钥并流式重写全部文件。

项目只保留 Work/Veil 目录模型和对应的 `.veil` 打包格式，两者使用同一套元数据和数据主密钥。密码强度工具验证包内加密元数据，不需要解包或读取文件内容。格式和恢复语义详见 [Core 文档](crates/veil-core/README.md)。

`.enc` 文件和加密元数据均通过认证加密识别密文损坏。

旧的独立单文件格式不再读取。若已有这类数据，应先使用支持该格式的旧版本导出内容，再用当前版本的 `init`、`add` 和 `pack` 重建；当前 `.veil` 打包文件无需转换。

## 构建与测试

开发环境需要 Rust 工具链和 Cargo。构建 CLI 使用 `cargo build --package veil-cli`，发布构建使用 `cargo build --release --package veil-cli`。

运行完整测试使用 `cargo test --workspace --all-targets`。生成库文档使用 `cargo doc --workspace --no-deps`。

桌面原型使用 React、TypeScript 和 Tauri，在 `apps/veil-app` 目录执行 `npm ci` 后运行 `npm run tauri dev`。原型已针对 macOS 构建；打开容器时选择包含 `.veil-info` 的目录，新建容器时会在所选位置创建独立目录。原型暂不登记 CLI 工作区，也暂不提供文件添加或导出操作。

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
