# Veil GUI

Veil 加密媒体保险箱的图形界面应用 —— 基于 Tauri + React 的桌面应用。

## 概述

`veil-gui` 是 Veil 项目的图形界面，提供直观易用的桌面应用体验。基于 Tauri 框架，使用 React + TypeScript 构建前端，调用 `veil-core` 库实现加密功能。

## 特性

- 🖥️ **跨平台**：支持 Windows、macOS、Linux
- 🎨 **现代界面**：基于 React + TypeScript
- 🚀 **性能优化**：Tauri 原生性能
- 🔐 **安全**：完全本地运行，无网络请求
- 📁 **文件管理**：拖拽上传、预览、导出
- 🌲 **目录树**：可视化文件组织
- 🔍 **搜索**：快速查找文件
- 📊 **统计信息**：容器大小、文件数、类型分布

## 架构

```
veil-gui/
├── src/                    // Rust 后端（Tauri）
│   ├── main.rs            // 主入口
│   └── commands/          // Tauri 命令
│       ├── container.rs   // 容器操作
│       └── file.rs        // 文件操作
├── src-ui/                // React 前端
│   ├── src/
│   │   ├── App.tsx       // 主应用
│   │   ├── components/   // React 组件
│   │   ├── hooks/        // 自定义 Hooks
│   │   └── types/        // TypeScript 类型
│   └── package.json
├── Cargo.toml            // Rust 依赖
└── tauri.conf.json       // Tauri 配置
```

## 技术栈

### 后端（Rust）
- **Tauri** - 应用框架
- **veil-core** - 核心加密库
- **tokio** - 异步运行时
- **serde** - 序列化/反序列化

### 前端（TypeScript）
- **React** - UI 框架
- **TypeScript** - 类型安全
- **Vite** - 构建工具
- **TailwindCSS** - 样式框架
- **React Query** - 数据管理
- **Zustand** - 状态管理

## 开发

### 环境要求

- Rust 1.70+
- Node.js 18+
- Tauri CLI

### 开发模式

```bash
# 安装依赖
cd src-ui
npm install

# 开发模式（热重载）
cd ..
cargo tauri dev
```

### 构建

```bash
# 构建生产版本
cargo tauri build

# 输出位置
# macOS: target/release/bundle/dmg/
# Windows: target/release/bundle/msi/
# Linux: target/release/bundle/deb/
```

## 功能

### 容器管理

- ✅ 创建新容器
- ✅ 打开现有容器
- ✅ 修改容器密码
- ✅ 查看容器信息
- ✅ 关闭容器

### 文件操作

- ✅ 添加文件（拖拽/选择）
- ✅ 添加目录
- ✅ 删除文件
- ✅ 重命名/移动文件
- ✅ 导出文件
- ✅ 导出目录
- ✅ 全部导出

### 查看功能

- ✅ 目录树视图
- ✅ 文件列表
- ✅ 文件预览（图片）
- ✅ 搜索文件
- ✅ 统计信息
- ✅ MIME 类型筛选

## 界面截图

### 主界面

```
┌─────────────────────────────────────────────────┐
│  Veil                                    ─ □ ✕  │
├─────────────────────────────────────────────────┤
│  photos.veil                      🔒 已解锁     │
├──────────┬──────────────────────────────────────┤
│          │  📁 2024/                            │
│  目录树  │    ├─ 📷 vacation.jpg   1.2 MB      │
│          │    ├─ 📷 family.jpg     0.8 MB      │
│  📁 2024 │    └─ 📷 friends.jpg    1.5 MB      │
│  📁 归档  │                                      │
│  📁 文档  │  📁 归档/                            │
│          │    └─ 📷 old-photo.jpg  0.5 MB      │
│          │                                      │
│          │  总计: 15 个文件, 25.6 MB           │
└──────────┴──────────────────────────────────────┘
```

## Tauri 命令

### 容器操作

```rust
#[tauri::command]
async fn create_container(path: String, password: String) -> Result<(), String>;

#[tauri::command]
async fn open_container(path: String, password: String) -> Result<ContainerInfo, String>;

#[tauri::command]
async fn close_container() -> Result<(), String>;

#[tauri::command]
async fn change_password(old_password: String, new_password: String) -> Result<(), String>;
```

### 文件操作

```rust
#[tauri::command]
async fn add_file(source: String, dest: String) -> Result<(), String>;

#[tauri::command]
async fn add_directory(source: String, dest: String) -> Result<(), String>;

#[tauri::command]
async fn remove_file(path: String) -> Result<(), String>;

#[tauri::command]
async fn rename_file(from: String, to: String) -> Result<(), String>;

#[tauri::command]
async fn extract_file(path: String, dest: String) -> Result<(), String>;

#[tauri::command]
async fn extract_all(dest: String) -> Result<(), String>;
```

### 查询操作

```rust
#[tauri::command]
async fn list_files() -> Result<Vec<FileInfo>, String>;

#[tauri::command]
async fn get_file_info(path: String) -> Result<FileInfo, String>;

#[tauri::command]
async fn get_container_info() -> Result<ContainerInfo, String>;

#[tauri::command]
async fn search_files(query: String) -> Result<Vec<FileInfo>, String>;
```

## 前端 API

### 容器钩子

```typescript
import { useContainer } from './hooks/useContainer';

function App() {
  const { 
    container, 
    createContainer, 
    openContainer, 
    closeContainer 
  } = useContainer();

  return (
    <div>
      {container ? (
        <ContainerView />
      ) : (
        <WelcomeScreen />
      )}
    </div>
  );
}
```

### 文件操作钩子

```typescript
import { useFiles } from './hooks/useFiles';

function FileList() {
  const { files, addFile, removeFile, exportFile } = useFiles();

  return (
    <div>
      {files.map(file => (
        <FileItem 
          key={file.path} 
          file={file}
          onDelete={() => removeFile(file.path)}
          onExport={() => exportFile(file.path)}
        />
      ))}
    </div>
  );
}
```

## 状态管理

使用 Zustand 管理全局状态：

```typescript
interface AppState {
  container: ContainerInfo | null;
  files: FileInfo[];
  selectedFiles: string[];
  // ...
}

const useStore = create<AppState>((set) => ({
  container: null,
  files: [],
  selectedFiles: [],
  // ...
}));
```

## 安全性

- ✅ **本地运行**：所有操作完全本地，无网络请求
- ✅ **密码保护**：密码不存储，仅在内存中
- ✅ **私钥管理**：容器关闭时清除私钥
- ✅ **沙箱隔离**：Tauri 提供的安全沙箱
- ✅ **文件权限**：用户明确授权才能访问文件

## 性能优化

- ✅ **懒加载**：按需加载文件列表
- ✅ **虚拟滚动**：大量文件时使用虚拟滚动
- ✅ **缓存**：文件元数据缓存
- ✅ **异步操作**：所有 I/O 操作异步执行
- ✅ **Web Worker**：计算密集型任务移到 Worker

## 键盘快捷键

| 快捷键 | 功能 |
|--------|------|
| `Ctrl/Cmd + N` | 新建容器 |
| `Ctrl/Cmd + O` | 打开容器 |
| `Ctrl/Cmd + W` | 关闭容器 |
| `Ctrl/Cmd + I` | 导入文件 |
| `Ctrl/Cmd + E` | 导出选中 |
| `Ctrl/Cmd + F` | 搜索文件 |
| `Delete` | 删除选中 |
| `F2` | 重命名 |

## 国际化

支持多语言（计划中）：

- 🇨🇳 简体中文
- 🇺🇸 English
- 🇯🇵 日本語

## 测试

```bash
# 前端测试
cd src-ui
npm test

# 后端测试
cargo test --package veil-gui
```

## 与 CLI 的关系

- **共享核心**：GUI 和 CLI 都使用 `veil-core`
- **容器兼容**：CLI 创建的容器可以在 GUI 中打开，反之亦然
- **互补使用**：GUI 适合日常使用，CLI 适合脚本和批处理

## 未来计划

- [ ] 文件预览（PDF、视频）
- [ ] 拖拽排序
- [ ] 批量操作
- [ ] 历史记录
- [ ] 自动备份
- [ ] 云同步（可选）
- [ ] 插件系统

## 许可证

待定
