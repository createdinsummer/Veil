# Veil 版本控制系统设计文档

## 1. 概述

### 1.1 目标

为 Veil 容器提供**类 Git 的版本控制能力**，支持：
- 容器内容的版本历史追踪
- 分支管理（多设备独立修改）
- 版本比较和合并
- 文件级别的版本跟踪

### 1.2 设计原则

- **精确到文件级别**：每个文件的变更独立跟踪
- **支持偏序关系**：不同分支的版本可能无法直接比较
- **去中心化**：每个设备可以独立创建版本
- **可选功能**：不启用版本控制时，性能无影响

---

## 2. 版本图模型

### 2.1 基本概念

**版本（Version）**：容器在某个时刻的完整状态快照

**版本图（Version Graph）**：有向无环图（DAG），表示版本之间的父子关系

```
示例版本图：

v1 ← v2 ← v3
      ↓
      v2.1 ← v2.2

节点：版本
边：父子关系（v2 是 v3 的父版本）
```

### 2.2 版本比较规则

**偏序关系**：

1. **直接祖先关系**：可比较
   ```
   v3 > v2 > v1
   v2.2 > v2.1 > v2
   ```

2. **分支关系**：不可比较
   ```
   v3 和 v2.2 无法比较（平行宇宙）
   ```

3. **比较算法**：
   ```
   compare(va, vb):
     if va 是 vb 的祖先:
       return vb > va
     else if vb 是 va 的祖先:
       return va > vb
     else:
       return INCOMPARABLE（需要合并）
   ```

---

## 3. 数据结构设计

### 3.1 版本对象（Version Object）

每个版本是一个不可变对象：

```rust
struct Version {
    // 版本标识
    id: VersionId,              // SHA-256 哈希
    
    // 版本关系
    parents: Vec<VersionId>,    // 父版本（通常 0-2 个）
    children: Vec<VersionId>,   // 子版本（运行时计算）
    
    // 版本元数据
    timestamp: DateTime<Utc>,   // 创建时间
    author: String,             // 作者（设备 ID 或用户名）
    message: String,            // 版本说明
    
    // 文件快照
    files: HashMap<String, FileVersion>,
}

struct FileVersion {
    path: String,               // 文件路径
    hash: String,               // 文件内容哈希（SHA-256）
    size: u64,                  // 文件大小
    encrypted_name: String,     // 加密文件名
    operation: FileOperation,   // 操作类型
}

enum FileOperation {
    Added,                      // 新增
    Modified,                   // 修改
    Deleted,                    // 删除
    Unchanged,                  // 未变化（优化存储）
}
```

### 3.2 版本 ID 生成

```rust
fn compute_version_id(version: &Version) -> VersionId {
    let mut hasher = Sha256::new();
    
    // 包含所有影响版本的因素
    hasher.update(version.timestamp.to_rfc3339());
    hasher.update(&version.author);
    hasher.update(&version.message);
    
    // 父版本 ID（保证唯一性）
    for parent_id in &version.parents {
        hasher.update(parent_id.as_bytes());
    }
    
    // 文件内容哈希
    for (path, file) in &version.files {
        hasher.update(path);
        hasher.update(&file.hash);
    }
    
    VersionId(hasher.finalize().to_vec())
}
```

**特点**：
- 内容寻址（Content-Addressable）
- 相同内容 = 相同 ID
- 防篡改

---

## 4. 版本存储

### 4.1 版本数据库

**位置**：`~/.veil/workspaces/default/myfiles/.veil-versions/`

**结构**：
```
.veil-versions/
  ├── objects/                  # 版本对象存储
  │   ├── a3/
  │   │   └── f2c1d4e5f6...    # 版本对象（按 ID 前两位分目录）
  │   └── b7/
  │       └── e4f9a8c3d2...
  │
  ├── refs/                     # 引用（分支/标签）
  │   ├── heads/
  │   │   ├── main             # 主分支
  │   │   └── device-macbook   # 设备分支
  │   └── tags/
  │       └── v1.0             # 标签
  │
  ├── HEAD                      # 当前版本指针
  └── config                    # 版本控制配置
```

### 4.2 版本对象格式

**文件名**：`objects/a3/f2c1d4e5f6...`

**内容**（JSON）：
```json
{
  "id": "a3f2c1d4e5f6...",
  "parents": ["b7e4f9a8c3d2..."],
  "timestamp": "2026-09-10T14:30:00Z",
  "author": "macbook-pro",
  "message": "添加 photo.jpg 和 notes.txt",
  "files": {
    "photo.jpg": {
      "hash": "sha256:abcd...",
      "size": 1024000,
      "encrypted_name": "a3f2c1d4.enc",
      "operation": "Added"
    },
    "notes.txt": {
      "hash": "sha256:1234...",
      "size": 2048,
      "encrypted_name": "b7e4f9a8.enc",
      "operation": "Added"
    }
  }
}
```

### 4.3 引用（Refs）

**HEAD**（当前版本）：
```
ref: refs/heads/main
```

**分支**（`refs/heads/main`）：
```
a3f2c1d4e5f6...
```

**标签**（`refs/tags/v1.0`）：
```
b7e4f9a8c3d2...
```

---

## 5. 核心操作

### 5.1 创建版本（Commit）

```bash
# 自动提交（每次 add/rm 后）
veil add photo.jpg myfiles.veil-link
→ 自动创建新版本

# 手动提交（批量修改后）
veil commit myfiles.veil-link -m "添加了多个文件"
```

**流程**：
```
1. 扫描工作区变更（对比 HEAD 版本）
2. 计算新增/修改/删除的文件
3. 创建版本对象
4. 计算版本 ID
5. 保存版本对象到 objects/
6. 更新 HEAD 指针
```

### 5.2 查看历史（Log）

```bash
# 查看版本历史
veil log myfiles.veil-link

# 输出示例
commit a3f2c1d4e5f6...
Author: macbook-pro
Date:   2026-09-10 14:30:00

    添加 photo.jpg 和 notes.txt

commit b7e4f9a8c3d2...
Author: macbook-pro
Date:   2026-09-10 10:00:00

    初始版本
```

### 5.3 版本比较（Diff）

```bash
# 比较两个版本
veil diff v1 v2 myfiles.veil-link

# 输出示例
+ photo.jpg     (added)
M notes.txt     (modified)
- oldfile.txt   (deleted)
```

### 5.4 回滚（Revert）

```bash
# 回滚到指定版本
veil revert a3f2c1d4 myfiles.veil-link

# 确认
警告：这将丢失当前版本之后的所有修改
当前版本：c9f1e8d7
目标版本：a3f2c1d4
确认回滚？[y/N]
```

### 5.5 分支管理

```bash
# 查看分支
veil branch myfiles.veil-link

# 输出
* main
  device-macbook
  device-ipad

# 创建分支
veil branch new-feature myfiles.veil-link

# 切换分支
veil checkout new-feature myfiles.veil-link

# 合并分支
veil merge device-ipad myfiles.veil-link
```

---

## 6. 多设备同步

### 6.1 场景

```
设备 A（MacBook）：
  v1 → v2 → v3

设备 B（iPad）：
  v1 → v2 → v2.1

同步后：
  v1 → v2 → v3
        ↓
        v2.1
```

### 6.2 同步流程

**1. 打包时包含版本信息**

```bash
# 设备 A
veil pack myfiles.veil-link -o myfiles.veil
→ 打包包含版本图
```

**2. 解包时检测分支**

```bash
# 设备 B
veil unpack myfiles.veil

检测到版本冲突：
- 本地版本：v2.1
- 远程版本：v3
- 共同祖先：v2

选项：
1. 保留本地版本（忽略远程）
2. 使用远程版本（覆盖本地）
3. 合并两个版本
```

**3. 合并策略**

```bash
# 自动合并（无冲突）
veil merge remote myfiles.veil-link

# 冲突处理
冲突文件：photo.jpg
- 本地版本：修改于 2026-09-10 10:00
- 远程版本：修改于 2026-09-10 11:00

选择：
1. 保留本地
2. 使用远程
3. 保留两者（重命名）
```

---

## 7. 版本图算法

### 7.1 祖先检测

```rust
fn is_ancestor(
    version_graph: &VersionGraph,
    ancestor: VersionId,
    descendant: VersionId
) -> bool {
    // BFS 搜索
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    
    queue.push_back(descendant);
    
    while let Some(current) = queue.pop_front() {
        if current == ancestor {
            return true;
        }
        
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current);
        
        // 访问父节点
        if let Some(version) = version_graph.get(&current) {
            for parent in &version.parents {
                queue.push_back(*parent);
            }
        }
    }
    
    false
}
```

### 7.2 最近公共祖先（LCA）

```rust
fn find_common_ancestor(
    version_graph: &VersionGraph,
    va: VersionId,
    vb: VersionId
) -> Option<VersionId> {
    // 找到两个版本的所有祖先
    let ancestors_a = get_all_ancestors(version_graph, va);
    let ancestors_b = get_all_ancestors(version_graph, vb);
    
    // 找交集
    let common: HashSet<_> = ancestors_a
        .intersection(&ancestors_b)
        .cloned()
        .collect();
    
    if common.is_empty() {
        return None;
    }
    
    // 找最近的（最晚的）公共祖先
    common.iter()
        .max_by_key(|id| {
            version_graph.get(id)
                .map(|v| v.timestamp)
                .unwrap_or(DateTime::MIN_UTC)
        })
        .cloned()
}
```

### 7.3 三路合并

```rust
fn three_way_merge(
    base: &Version,    // 共同祖先
    ours: &Version,    // 本地版本
    theirs: &Version,  // 远程版本
) -> MergeResult {
    let mut conflicts = Vec::new();
    let mut merged_files = HashMap::new();
    
    // 遍历所有文件
    let all_files: HashSet<_> = base.files.keys()
        .chain(ours.files.keys())
        .chain(theirs.files.keys())
        .collect();
    
    for path in all_files {
        let base_file = base.files.get(path);
        let our_file = ours.files.get(path);
        let their_file = theirs.files.get(path);
        
        match (base_file, our_file, their_file) {
            // 双方都没修改
            (Some(b), Some(o), Some(t)) if o.hash == b.hash && t.hash == b.hash => {
                merged_files.insert(path, b.clone());
            }
            
            // 只有我们修改
            (Some(b), Some(o), Some(t)) if o.hash != b.hash && t.hash == b.hash => {
                merged_files.insert(path, o.clone());
            }
            
            // 只有他们修改
            (Some(b), Some(o), Some(t)) if o.hash == b.hash && t.hash != b.hash => {
                merged_files.insert(path, t.clone());
            }
            
            // 双方都修改 → 冲突
            (Some(_), Some(o), Some(t)) if o.hash != t.hash => {
                conflicts.push(MergeConflict {
                    path: path.clone(),
                    ours: o.clone(),
                    theirs: t.clone(),
                });
            }
            
            // ... 其他情况
        }
    }
    
    MergeResult {
        merged_files,
        conflicts,
    }
}
```

---

## 8. 性能优化

### 8.1 增量存储

**问题**：每个版本都存储完整文件列表，浪费空间

**优化**：只存储变化的文件

```json
{
  "id": "a3f2c1d4...",
  "parents": ["b7e4f9a8..."],
  "delta": {
    "added": ["photo.jpg"],
    "modified": ["notes.txt"],
    "deleted": ["oldfile.txt"]
  },
  "unchanged": ["music.mp3", "video.mp4"]
}
```

### 8.2 版本压缩

**策略**：定期合并旧版本

```bash
# 压缩 30 天前的版本
veil gc --before 30d myfiles.veil-link

# 结果
v1 → v2 → v3 → ... → v50 → v51 → v52
合并为：
v1 → v50 → v51 → v52
```

### 8.3 浅克隆

**场景**：从远程解包时，不需要完整历史

```bash
# 只解包最近 10 个版本
veil unpack myfiles.veil --depth 10
```

---

## 9. 配置选项

### 9.1 启用版本控制

```toml
# myfiles.veil-link
[versioning]
enabled = true
auto_commit = true          # 每次 add/rm 自动提交
commit_message = "Auto commit"
max_versions = 100          # 保留最近 100 个版本
gc_policy = "time"          # time | count | size
gc_threshold = "30d"        # 30 天前的版本可以压缩
```

### 9.2 全局配置

```toml
# ~/.veil/config.toml
[versioning.defaults]
enabled = false             # 默认不启用（性能优先）
auto_commit = true
author = "macbook-pro"      # 默认作者
```

---

## 10. 命令行接口

### 10.1 版本管理命令

```bash
# 启用版本控制
veil init myfiles --versioning

# 查看版本历史
veil log myfiles.veil-link
veil log myfiles.veil-link --graph     # 图形化显示

# 查看版本详情
veil show a3f2c1d4 myfiles.veil-link

# 比较版本
veil diff v1 v2 myfiles.veil-link
veil diff HEAD~1 HEAD myfiles.veil-link

# 回滚
veil revert a3f2c1d4 myfiles.veil-link
veil reset --hard HEAD~1 myfiles.veil-link

# 标签
veil tag v1.0 myfiles.veil-link
veil tag list myfiles.veil-link

# 分支
veil branch myfiles.veil-link
veil branch new-feature myfiles.veil-link
veil checkout new-feature myfiles.veil-link
veil merge device-ipad myfiles.veil-link
```

---

## 11. 使用场景

### 11.1 场景 1：本地历史追踪

```bash
# 用户操作
veil add photo.jpg myfiles.veil-link    # v1
veil add notes.txt myfiles.veil-link    # v2
veil rm photo.jpg myfiles.veil-link     # v3

# 查看历史
veil log myfiles.veil-link

# 恢复误删的文件
veil revert v2 myfiles.veil-link
```

### 11.2 场景 2：多设备同步

```bash
# MacBook
veil add file1.txt myfiles.veil-link    # v1 → v2
veil pack myfiles.veil-link -o sync.veil

# iPad（从 v1 开始）
veil add file2.txt myfiles.veil-link    # v1 → v1.1
veil unpack sync.veil

# 检测到分支
分支：
  main: v2 (MacBook)
  device-ipad: v1.1 (本地)

合并策略：
  file1.txt (从 v2)
  file2.txt (从 v1.1)
→ 创建合并版本 v3
```

### 11.3 场景 3：实验性修改

```bash
# 创建实验分支
veil branch experiment myfiles.veil-link
veil checkout experiment myfiles.veil-link

# 尝试修改
veil add test.txt myfiles.veil-link

# 不满意，切回主分支
veil checkout main myfiles.veil-link

# 满意，合并
veil merge experiment myfiles.veil-link
```

---

## 12. 实现优先级

### Phase 1：基础版本控制（P0）
- [ ] 版本对象数据结构
- [ ] 版本 ID 生成
- [ ] 版本对象存储（objects/）
- [ ] 基础命令：commit, log, show

### Phase 2：版本比较（P1）
- [ ] 版本图构建
- [ ] 祖先检测算法
- [ ] diff 命令
- [ ] revert 命令

### Phase 3：分支管理（P2）
- [ ] 引用系统（refs/）
- [ ] branch 命令
- [ ] checkout 命令
- [ ] 最近公共祖先算法

### Phase 4：合并支持（P3）
- [ ] 三路合并算法
- [ ] 冲突检测
- [ ] merge 命令
- [ ] 冲突解决界面

### Phase 5：性能优化（P4）
- [ ] 增量存储
- [ ] 版本压缩（GC）
- [ ] 浅克隆

---

## 13. 待讨论问题

### 13.1 版本策略

- [ ] 默认启用版本控制？还是可选？
  - **建议**：可选，默认关闭（性能优先）

- [ ] 自动提交频率？
  - 选项 1：每次 add/rm 自动提交
  - 选项 2：手动提交（git 风格）
  - **建议**：可配置，默认自动提交

### 13.2 存储策略

- [ ] 版本对象压缩？
  - 选项 1：明文 JSON（便于调试）
  - 选项 2：压缩 JSON（节省空间）
  - **建议**：使用 gzip 压缩

### 13.3 合并策略

- [ ] 文件冲突如何处理？
  - 选项 1：保留两者（重命名）
  - 选项 2：交互式选择
  - 选项 3：最新优先
  - **建议**：交互式选择 + 可配置默认策略

---

## 14. 技术挑战

### 14.1 性能

- **问题**：版本历史增长后，查询变慢
- **方案**：
  - 版本索引
  - 缓存祖先关系
  - 限制历史深度

### 14.2 存储空间

- **问题**：每个版本存储完整文件列表，空间占用大
- **方案**：
  - 增量存储（只存变化）
  - 定期 GC
  - 文件去重（相同内容只存一次）

### 14.3 并发安全

- **问题**：多个进程同时修改版本图
- **方案**：
  - 文件锁
  - 原子操作
  - 冲突检测

---

## 附录 A：版本图示例

### 示例 1：线性历史

```
v1 ← v2 ← v3 ← v4 ← v5
(main)

操作：
- v1: 初始版本
- v2: 添加 photo.jpg
- v3: 修改 notes.txt
- v4: 删除 oldfile.txt
- v5: 添加 music.mp3
```

### 示例 2：分支历史

```
v1 ← v2 ← v3 ← v4
      ↓
      v2.1 ← v2.2 ← v2.3

分支：
- main: v1 → v2 → v3 → v4
- device-ipad: v1 → v2 → v2.1 → v2.2 → v2.3
```

### 示例 3：合并后的历史

```
v1 ← v2 ← v3 ← v4 ← v5
      ↓            ↗
      v2.1 ← v2.2 ← (merge)

操作：
- v5 = merge(v4, v2.2)
- v5 的父版本：[v4, v2.2]
```

---

## 附录 B：与 Git 的对比

| 特性 | Git | Veil |
|------|-----|------|
| **粒度** | 文件 | 文件 |
| **版本 ID** | SHA-1 | SHA-256 |
| **分支** | 轻量级 | 轻量级 |
| **合并** | 三路合并 | 三路合并 |
| **存储** | objects/ | objects/ |
| **引用** | refs/ | refs/ |
| **区别** | 代码管理 | 加密文件管理 |

---

**文档版本**：1.0  
**创建日期**：2026-09-10  
**状态**：需求设计阶段  
**实现优先级**：Phase 6+（可选功能）
