# Veil 暴力破解工具使用指南

⚠️ **重要声明**: 此工具仅供教育目的和测试自己创建的容器使用。请勿用于攻击他人的数据。

## 功能说明

这个暴力破解工具用于：
1. **评估密码强度** - 测试你的密码是否容易被破解
2. **验证 scrypt 保护** - 演示 scrypt 工作因子如何减缓暴力破解
3. **安全教育** - 理解弱密码的危险性

## 攻击模式

### 1. 常见密码模式攻击
测试最常见的弱密码：
- 超弱密码: `password`, `123456`, `qwerty` 等
- 生日模式: `19900101`, `20000101` 等
- 简单组合: `admin123`, `test123` 等
- 键盘序列: `qwertyuiop`, `1q2w3e4r` 等

### 2. 纯数字暴力破解
尝试所有 4-8 位数字组合：
- 4 位: 0000-9999 (10,000 种)
- 5 位: 10000-99999 (90,000 种)
- 6 位: 100000-999999 (900,000 种)
- 更长位数会指数级增长

### 3. 字典攻击
从单词表文件逐行读取密码尝试。

## 使用方法

### 基础用法

```bash
# Release 模式编译（推荐）
cargo run -p veil-brute-force --release

# 或先编译再运行
cargo build -p veil-brute-force --release
./target/release/veil-brute-force

# 使用字典文件
./target/release/veil-brute-force
```

## 性能说明

暴力破解速度主要取决于 **scrypt 工作因子**：

| 容器创建模式 | scrypt work_factor | 尝试速度 | 破解难度 |
|------------|-------------------|---------|----------|
| Debug 构建 | 12 | ~50-100 次/秒 | 较低 |
| Release 构建 | 18 | ~1-5 次/秒 | 高 64 倍 |

### 实际示例

假设密码是 6 位纯数字 (1,000,000 种可能)：

- **Debug 容器 (work_factor=12)**
  - 速度: 50 次/秒
  - 平均破解时间: 1,000,000 / 50 / 2 = 10,000 秒 ≈ 2.8 小时

- **Release 容器 (work_factor=18)**
  - 速度: 2 次/秒
  - 平均破解时间: 1,000,000 / 2 / 2 = 250,000 秒 ≈ 69 小时

**结论**: Release 模式的 scrypt 因子将暴力破解速度降低了 25-50 倍！

## 创建测试容器

### 弱密码容器（用于演示破解）

```bash
# 进入 veil-cli 目录
cd crates/veil-cli

# 创建一个使用弱密码的测试容器
cargo run --release -- create test_weak.veil
# 输入密码: 123456

# 添加一些测试文件
echo "test content" > test.txt
cargo run --release -- add test_weak.veil test.txt
```

### 强密码容器（演示防护）

```bash
cargo run --release -- create test_strong.veil
# 输入密码: Tr0ub4dor&3_xK9#mQ2$pL7
```

## 破解测试

### 测试 1: 破解弱密码容器

```bash
cargo run -p veil-brute-force --release
# 然后在交互界面中输入容器路径
```

预期结果: 在几秒钟内破解成功（因为 123456 在常见密码列表中）

### 测试 2: 尝试破解强密码容器

```bash
cargo run -p veil-brute-force --release
# 输入强密码容器路径
```

预期结果: 无法破解（密码不在常见模式和数字范围内）

## 创建自定义单词表

### 示例单词表 (top_passwords.txt)

```text
password
123456
12345678
qwerty
abc123
monkey
letmein
trustno1
dragon
baseball
iloveyou
master
sunshine
ashley
bailey
shadow
superman
```

### 使用单词表

```bash
cargo run -p veil-brute-force --release
# 在交互界面中选择"字典攻击"，然后输入单词表路径
```

## 防御建议

### ✅ 好密码特征
1. **长度**: 至少 12 个字符
2. **复杂度**: 混合大小写、数字、符号
3. **随机性**: 不是常见单词或模式
4. **唯一性**: 每个容器使用不同密码

### ❌ 坏密码示例
- `password`, `123456`, `qwerty` - 超弱密码
- `19900101`, `20000101` - 生日模式
- `admin123`, `test123` - 简单组合
- `aaaa`, `1111` - 重复字符

### 推荐密码生成方法

#### 1. 密码短语（Diceware）
```
correct-horse-battery-staple
```
优点: 易记、足够长、随机

#### 2. 随机密码生成器
```bash
# macOS/Linux
openssl rand -base64 20

# 或使用密码管理器生成
```

#### 3. 混合方法
```
MyF@v0rite_B00k$2024!
```
基于记忆点 + 替换 + 符号

## 技术细节

### Scrypt 参数

Veil 使用的 scrypt KDF 参数：
- **work_factor (N)**: 2^18 (release) 或 2^12 (debug)
- **r**: 8
- **p**: 1

work_factor 每增加 1，计算时间翻倍。从 12 到 18 意味着增加了 2^6 = 64 倍的计算成本。

### Age 加密

- **算法**: X25519 (密钥交换) + ChaCha20-Poly1305 (AEAD)
- **认证**: 每个 blob 都有认证标签，篡改会被立即检测
- **密钥模型**: 容器密钥独立，密码只用于保护容器私钥

## 扩展攻击方法

如果你想进一步测试密码强度，可以扩展这个工具：

### 1. 字符集暴力破解
```rust
// 添加到 brute_force.rs
fn charset_attack(
    container_path: &Path,
    charset: &str,  // 如 "0123456789abcdefghijklmnopqrstuvwxyz"
    min_len: usize,
    max_len: usize,
    stats: Arc<Stats>,
) -> Option<String> {
    // 实现递归或迭代生成所有组合
    // 警告: 复杂度为 charset.len()^max_len
}
```

### 2. 规则引擎（类似 Hashcat）
```rust
// 常见规则: 首字母大写、末尾加数字、leet speak 替换
fn apply_rules(word: &str) -> Vec<String> {
    vec![
        word.to_string(),
        capitalize_first(word),
        format!("{word}123"),
        format!("{word}!"),
        leet_speak(word),
    ]
}
```

### 3. 并行攻击
```rust
use rayon::prelude::*;

// 使用多线程并行尝试
// 注意: Veil 的 scrypt 已经是 CPU 密集型，并行收益有限
```

## 实验与学习

### 实验 1: 测量 scrypt 成本

创建两个容器，分别在 debug 和 release 模式：

```bash
# Debug 模式容器
cargo build
./target/debug/veil-cli create test_debug.veil  # 密码: test

# Release 模式容器  
cargo build --release
./target/release/veil-cli create test_release.veil  # 密码: test

# 测量破解速度差异
time cargo run -p veil-brute-force --release
# 分别测试两个容器
```

### 实验 2: 密码熵分析

计算密码的理论搜索空间：

| 字符集 | 大小 | 8 位密码空间 |
|--------|------|-------------|
| 纯数字 | 10 | 10^8 = 100M |
| 小写字母 | 26 | 26^8 ≈ 208B |
| 字母+数字 | 36 | 36^8 ≈ 2.8T |
| 字母+数字+符号 | 70 | 70^8 ≈ 576T |

**关键启示**: 字符集的多样性比长度更重要！

## 结论

这个工具演示了：
1. **弱密码极易被破解** - 常见密码在秒级被破解
2. **Scrypt 是有效防护** - 大幅减缓暴力破解速度
3. **密码强度很重要** - 长随机密码是最佳防御

**记住**: 
- 永远使用强密码
- Release 模式创建容器
- 定期更换密码
- 不要重复使用密码

---

**免责声明**: 此工具仅用于教育和测试自己的容器。未经授权访问他人数据是违法行为。
