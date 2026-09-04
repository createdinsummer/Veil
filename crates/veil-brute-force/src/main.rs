//! # Veil 容器暴力破解工具（交互式多线程版）
//!
//! **仅供教育目的和测试自己创建的容器使用**
//!
//! ## 功能特性
//!
//! 1. **字典攻击**：从文件读取密码列表（多线程）
//! 2. **自定义字符集暴力破解**：完全自定义（多线程）
//! 3. **交互式配置**：友好的命令行交互界面
//! 4. **多线程加速**：利用 CPU 多核心并行破解
//!
//! ## 多线程说明
//!
//! - 使用 Rayon 进行并行处理
//! - 自动检测 CPU 核心数
//! - 可配置线程数（默认使用所有核心）
//! - 线程安全的密钥共享（Arc）

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::collections::HashSet;

use age::secrecy::SecretString;
use rayon::prelude::*;
use veil_core::{format, keys};

/// 版本信息
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 历史密码文件目录
const HISTORY_DIR: &str = ".veil_history";

/// 将容器路径转换为历史文件名
/// 例如: /Users/mac/test.veil -> Users.mac.test.veil.txt
fn container_path_to_history_filename(container_path: &Path) -> String {
    let abs_path = std::fs::canonicalize(container_path)
        .unwrap_or_else(|_| container_path.to_path_buf());

    let path_str = abs_path.to_string_lossy();

    // 移除开头的 / 或 Windows 的盘符
    let cleaned = if path_str.starts_with('/') {
        &path_str[1..]
    } else if path_str.len() > 2 && path_str.chars().nth(1) == Some(':') {
        // Windows: C:\path -> path
        &path_str[3..]
    } else {
        &*path_str
    };

    // 将 / 和 \ 替换为 .
    let filename = cleaned.replace('/', ".").replace('\\', ".");

    format!("{}.txt", filename)
}

/// 获取历史密码文件路径
fn get_history_file_path(container_path: &Path) -> std::path::PathBuf {
    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());

    let history_dir = Path::new(&home_dir).join(HISTORY_DIR);

    // 确保目录存在
    std::fs::create_dir_all(&history_dir).ok();

    let filename = container_path_to_history_filename(container_path);
    history_dir.join(filename)
}

/// 加载历史密码
fn load_history(container_path: &Path) -> HashSet<String> {
    let history_file = get_history_file_path(container_path);

    if !history_file.exists() {
        return HashSet::new();
    }

    let file = match File::open(&history_file) {
        Ok(f) => f,
        Err(_) => return HashSet::new(),
    };

    BufReader::new(file)
        .lines()
        .filter_map(|line| line.ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 保存已尝试的密码到历史
fn save_to_history(container_path: &Path, passwords: &[String]) {
    let history_file = get_history_file_path(container_path);

    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_file)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("⚠️  无法保存历史: {}", e);
            return;
        }
    };

    for password in passwords {
        if let Err(e) = writeln!(file, "{}", password) {
            eprintln!("⚠️  写入历史失败: {}", e);
            break;
        }
    }
}

/// 显示历史统计
fn show_history_stats(container_path: &Path) {
    let history = load_history(container_path);
    let history_file = get_history_file_path(container_path);

    if history.is_empty() {
        println!("📝 历史记录: 无");
    } else {
        println!("📝 历史记录: {} 个已尝试的密码", history.len());
        println!("   文件位置: {}", history_file.display());
    }
}

/// 重试历史密码
fn retry_history_passwords(cip_pri_key: Arc<Vec<u8>>, container_path: &Path, stats: Arc<Stats>) -> Option<String> {
    println!("\n🔄 重试历史密码");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let history = load_history(container_path);

    if history.is_empty() {
        println!("❌ 没有历史密码记录");
        return None;
    }

    let passwords: Vec<String> = history.into_iter().collect();
    println!("历史密码数: {}", passwords.len());
    println!("线程数: {} (CPU 核心数)", rayon::current_num_threads());

    println!("\n⚠️  这将重新尝试所有历史密码");
    print!("确认开始？(y/N): ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    if input.trim().to_lowercase() != "y" {
        println!("已取消");
        return None;
    }

    println!("\n开始尝试...");
    println!("🚀 多线程模式：{} 个并行任务", rayon::current_num_threads());
    println!();

    // 启动进度监控线程
    let stats_clone = Arc::clone(&stats);
    let total_combos = passwords.len();
    let monitor_handle = std::thread::spawn(move || {
        let mut last_attempts = 0;
        while stats_clone.is_running() && !stats_clone.is_found() {
            std::thread::sleep(Duration::from_secs(2));
            if !stats_clone.is_running() || stats_clone.is_found() {
                break;
            }
            let attempts = stats_clone.get_attempts();
            let elapsed = stats_clone.elapsed().as_secs_f64();
            let rate = attempts as f64 / elapsed;
            let speed_per_2s = attempts.saturating_sub(last_attempts);
            last_attempts = attempts;

            // 计算进度
            let progress = (attempts as f64 / total_combos as f64 * 100.0).min(100.0);
            let remaining = total_combos.saturating_sub(attempts as usize);
            let eta_seconds = if rate > 0.0 {
                remaining as f64 / rate
            } else {
                0.0
            };

            // 格式化剩余时间
            let eta_str = if eta_seconds < 60.0 {
                format!("{}秒", eta_seconds as u64)
            } else if eta_seconds < 3600.0 {
                format!("{:.1}分钟", eta_seconds / 60.0)
            } else if eta_seconds < 86400.0 {
                format!("{:.1}小时", eta_seconds / 3600.0)
            } else {
                format!("{:.1}天", eta_seconds / 86400.0)
            };

            // 生成进度条
            let bar_width = 30;
            let filled = (progress / 100.0 * bar_width as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);

            println!(
                "📊 [{}] {:.1}% | {}/{} | 速度 {:.1}/s | 最近2s: {} | ⏱️  剩余 {}",
                bar, progress, attempts, total_combos, rate, speed_per_2s, eta_str
            );
        }
    });

    // 多线程并行搜索
    let result = passwords
        .par_iter()
        .find_map_any(|password| {
            if !stats.is_running() || stats.is_found() {
                return None;
            }

            stats.increment();

            if try_password(&cip_pri_key, password) {
                stats.mark_found();
                stats.stop();
                Some(password.clone())
            } else {
                None
            }
        });

    stats.stop();
    monitor_handle.join().ok();

    result
}

/// 暴力破解统计信息
struct Stats {
    attempts: AtomicU64,
    start_time: Instant,
    running: AtomicBool,
    found: AtomicBool,
}

impl Stats {
    fn new() -> Self {
        Stats {
            attempts: AtomicU64::new(0),
            start_time: Instant::now(),
            running: AtomicBool::new(true),
            found: AtomicBool::new(false),
        }
    }

    fn increment(&self) {
        self.attempts.fetch_add(1, Ordering::Relaxed);
    }

    fn get_attempts(&self) -> u64 {
        self.attempts.load(Ordering::Relaxed)
    }

    fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    fn mark_found(&self) {
        self.found.store(true, Ordering::Relaxed);
    }

    fn is_found(&self) -> bool {
        self.found.load(Ordering::Relaxed)
    }
}

/// 尝试用给定密码解密容器私钥
fn try_password(cip_pri_key: &[u8], password: &str) -> bool {
    keys::decrypt_pri_key(cip_pri_key, SecretString::from(password.to_owned())).is_ok()
}

/// 字典攻击（多线程版）
fn dictionary_attack(cip_pri_key: Arc<Vec<u8>>, wordlist_path: &Path, container_path: &Path, stats: Arc<Stats>) -> Option<String> {
    println!("\n🔍 字典攻击模式（多线程）");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("单词表: {}", wordlist_path.display());
    println!("线程数: {} (CPU 核心数)", rayon::current_num_threads());

    let file = match File::open(wordlist_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("❌ 无法打开单词表: {e}");
            return None;
        }
    };

    // 读取所有密码到内存（为了并行处理）
    let mut passwords: Vec<String> = BufReader::new(file)
        .lines()
        .filter_map(|line| line.ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    println!("密码总数: {}", passwords.len());
    let original_count = passwords.len();
    println!("密码总数: {}", original_count);

    // 加载历史并过滤
    let history = load_history(container_path);
    if !history.is_empty() {
        passwords.retain(|p| !history.contains(p));
        let filtered = original_count - passwords.len();

        if filtered > 0 {
            println!("📝 已跳过 {} 个历史密码", filtered);
            println!("   剩余 {} 个新密码", passwords.len());
        }

        if passwords.is_empty() {
            println!("❌ 所有密码都已尝试过");
            return None;
        }
    }

    println!("开始尝试...");
    println!("🚀 多线程模式：{} 个并行任务", rayon::current_num_threads());
    println!();
    let stats_clone = Arc::clone(&stats);
    let monitor_handle = std::thread::spawn(move || {
        let mut last_attempts = 0;
        while stats_clone.is_running() && !stats_clone.is_found() {
            std::thread::sleep(Duration::from_secs(2));
            if !stats_clone.is_running() || stats_clone.is_found() {
                break;
            }
            let attempts = stats_clone.get_attempts();
            let elapsed = stats_clone.elapsed().as_secs_f64();
            let rate = attempts as f64 / elapsed;
            let speed_per_2s = attempts.saturating_sub(last_attempts);
            last_attempts = attempts;

            println!(
                "📊 [监控] 尝试 {:>8} 次 | 速度 {:>5.1} 次/秒 | 最近2秒完成: {} 次 ⚡",
                attempts, rate, speed_per_2s
            );
        }
    });

    // 多线程并行搜索
    let result = passwords
        .par_iter()
        .find_map_any(|password| {
            if !stats.is_running() || stats.is_found() {
                return None;
            }

            stats.increment();

            if try_password(&cip_pri_key, password) {
                stats.mark_found();
                stats.stop();
                Some(password.clone())
            } else {
                None
            }
        });

    stats.stop();

    // 保存历史
    save_to_history(container_path, &passwords);
    monitor_handle.join().ok();

    result
}

/// 生成指定长度的所有字符组合（迭代版，用于多线程）
fn generate_all_combinations(charset: &[char], length: usize) -> Vec<String> {
    if length == 0 {
        return vec![String::new()];
    }

    let mut results = Vec::new();
    let mut indices = vec![0usize; length];

    loop {
        // 生成当前组合
        let combination: String = indices.iter().map(|&i| charset[i]).collect();
        results.push(combination);

        // 递增索引（类似进位）
        let mut pos = length - 1;
        loop {
            indices[pos] += 1;
            if indices[pos] < charset.len() {
                break;
            }
            indices[pos] = 0;
            if pos == 0 {
                return results; // 所有组合生成完毕
            }
            pos -= 1;
        }
    }
}

/// 自定义字符集暴力破解（多线程版）
fn charset_attack(
    cip_pri_key: Arc<Vec<u8>>,
    charset: &str,
    min_len: usize,
    max_len: usize,
    stats: Arc<Stats>,
) -> Option<String> {
    let chars: Vec<char> = charset.chars().collect();

    println!("\n🔡 字符集暴力破解模式（多线程）");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("字符集: {}", charset);
    println!("字符数: {}", chars.len());
    println!("长度范围: {}-{} 位", min_len, max_len);
    println!("线程数: {} (CPU 核心数)", rayon::current_num_threads());

    // 计算搜索空间
    let mut total_combinations = 0u64;
    for len in min_len..=max_len {
        if let Some(count) = (chars.len() as u64).checked_pow(len as u32) {
            total_combinations = total_combinations.saturating_add(count);
        } else {
            total_combinations = u64::MAX;
            break;
        }
    }

    if total_combinations == u64::MAX {
        println!("搜索空间: 超大（> 2^64）");
    } else {
        println!("搜索空间: {} 种组合", total_combinations);

        // 估算时间（多线程加速，假设线性加速）
        let threads = rayon::current_num_threads() as f64;
        let estimated_seconds = total_combinations as f64 / (0.2 * threads);

        print!("预估时间（单线程）: ");
        print_time_estimate(total_combinations as f64 / 0.2);

        print!("预估时间（{}线程）: ", threads as usize);
        print_time_estimate(estimated_seconds);
    }

    println!("\n⚠️  警告: 搜索空间很大可能需要很长时间！");
    print!("确认开始？(y/N): ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    if input.trim().to_lowercase() != "y" {
        println!("已取消");
        return None;
    }

    println!("\n开始尝试...");
    println!("🚀 多线程模式：{} 个并行任务", rayon::current_num_threads());
    println!();

    // 启动进度监控线程
    let stats_clone = Arc::clone(&stats);
    let total_combos = total_combinations as usize;
    let monitor_handle = std::thread::spawn(move || {
        let mut last_attempts = 0;
        while stats_clone.is_running() && !stats_clone.is_found() {
            std::thread::sleep(Duration::from_secs(2));
            if !stats_clone.is_running() || stats_clone.is_found() {
                break;
            }
            let attempts = stats_clone.get_attempts();
            let elapsed = stats_clone.elapsed().as_secs_f64();
            let rate = attempts as f64 / elapsed;
            let speed_per_2s = attempts.saturating_sub(last_attempts);
            last_attempts = attempts;

            // 计算进度
            let progress = (attempts as f64 / total_combos as f64 * 100.0).min(100.0);
            let remaining = total_combos.saturating_sub(attempts as usize);
            let eta_seconds = if rate > 0.0 {
                remaining as f64 / rate
            } else {
                0.0
            };

            // 格式化剩余时间
            let eta_str = if eta_seconds < 60.0 {
                format!("{}秒", eta_seconds as u64)
            } else if eta_seconds < 3600.0 {
                format!("{:.1}分钟", eta_seconds / 60.0)
            } else if eta_seconds < 86400.0 {
                format!("{:.1}小时", eta_seconds / 3600.0)
            } else {
                format!("{:.1}天", eta_seconds / 86400.0)
            };

            // 生成进度条
            let bar_width = 30;
            let filled = (progress / 100.0 * bar_width as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);

            println!(
                "📊 [{}] {:.1}% | {}/{} | 速度 {:.1}/s | 最近2s: {} | ⏱️  剩余 {}",
                bar, progress, attempts, total_combos, rate, speed_per_2s, eta_str
            );
        }
    });

    let mut result = None;

    for length in min_len..=max_len {
        if !stats.is_running() || stats.is_found() {
            break;
        }

        println!("尝试 {} 位密码...", length);

        // 生成所有组合
        let combinations = generate_all_combinations(&chars, length);
        println!("  组合数: {}", combinations.len());

        // 多线程并行搜索
        let found = combinations
            .par_iter()
            .find_map_any(|password| {
                if !stats.is_running() || stats.is_found() {
                    return None;
                }

                stats.increment();

                if try_password(&cip_pri_key, password) {
                    stats.mark_found();
                    stats.stop();
                    Some(password.clone())
                } else {
                    None
                }
            });

        if let Some(password) = found {
            result = Some(password);
            break;
        }
    }

    stats.stop();
    monitor_handle.join().ok();

    result
}

/// 格式化时间估算
fn print_time_estimate(seconds: f64) {
    if seconds < 60.0 {
        println!("{:.1} 秒", seconds);
    } else if seconds < 3600.0 {
        println!("{:.1} 分钟", seconds / 60.0);
    } else if seconds < 86400.0 {
        println!("{:.1} 小时", seconds / 3600.0);
    } else if seconds < 31536000.0 {
        println!("{:.1} 天", seconds / 86400.0);
    } else {
        println!("{:.1} 年", seconds / 31536000.0);
    }
}

/// 显示主菜单并获取用户选择
fn show_menu() -> String {
    println!("\n╔════════════════════════════════════════════╗");
    println!("║  Veil 容器暴力破解工具（多线程增强版）     ║");
    println!("╚════════════════════════════════════════════╝");
    println!("\n请选择攻击模式：");
    println!("  1. 字典攻击（从文件读取密码列表）");
    println!("  2. 自定义字符集暴力破解");
    println!("  3. 预设字符集快速选择");
    println!("  4. 组词攻击（单词片段组合）");
    println!("  5. 配置线程数");
    println!("  6. 切换容器文件");
    println!("  7. 重试历史密码");
    println!("  0. 退出");
    print!("\n请输入选项 [0-7]: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    input.trim().to_string()
}

/// 获取字典文件路径
fn get_wordlist_path() -> Option<String> {
    println!("\n请输入单词表文件路径:");
    println!("  示例: common_passwords.txt");
    println!("  示例: crates/veil-brute-force/common_passwords.txt");
    print!("  路径: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let path = input.trim().to_string();

    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// 获取自定义字符集
fn get_custom_charset() -> Option<(String, usize, usize)> {
    println!("\n自定义字符集配置");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    println!("\n请选择要包含的字符类型（可多选，用空格分隔）:");
    println!("  1. 小写字母 (a-z)");
    println!("  2. 大写字母 (A-Z)");
    println!("  3. 数字 (0-9)");
    println!("  4. 特殊符号 (!@#$%^&*...)");
    println!("  5. 自定义输入字符");
    print!("\n请输入选项（如: 1 3 表示小写+数字）: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let selections: Vec<&str> = input.trim().split_whitespace().collect();

    let mut charset = String::new();

    for sel in selections {
        match sel {
            "1" => charset.push_str("abcdefghijklmnopqrstuvwxyz"),
            "2" => charset.push_str("ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            "3" => charset.push_str("0123456789"),
            "4" => charset.push_str("!@#$%^&*()-_=+[]{}|;:',.<>?/~`"),
            "5" => {
                print!("请输入自定义字符: ");
                io::stdout().flush().ok();
                let mut custom = String::new();
                io::stdin().read_line(&mut custom).ok();
                charset.push_str(custom.trim());
            }
            _ => {}
        }
    }

    if charset.is_empty() {
        println!("❌ 未选择任何字符集");
        return None;
    }

    // 去重
    let mut chars: Vec<char> = charset.chars().collect();
    chars.sort_unstable();
    chars.dedup();
    charset = chars.into_iter().collect();

    println!("\n最终字符集: {}", charset);
    println!("字符总数: {}", charset.len());

    // 获取密码长度范围
    print!("\n请输入最小密码长度 (默认 1): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let min_len = input.trim().parse::<usize>().unwrap_or(1).max(1);

    print!("请输入最大密码长度 (默认 4): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let max_len = input.trim().parse::<usize>().unwrap_or(4).max(min_len);

    Some((charset, min_len, max_len))
}

/// 预设字符集快速选择
fn get_preset_charset() -> Option<(String, usize, usize)> {
    println!("\n预设字符集快速选择");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("\n请选择预设:");
    println!("  1. 纯数字 (0-9)");
    println!("  2. 纯小写字母 (a-z)");
    println!("  3. 小写字母+数字 (a-z, 0-9)");
    println!("  4. 大小写字母+数字 (A-Z, a-z, 0-9)");
    println!("  5. 全字符（字母+数字+符号）");
    print!("\n请输入选项 [1-5]: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();

    let charset = match input.trim() {
        "1" => "0123456789".to_string(),
        "2" => "abcdefghijklmnopqrstuvwxyz".to_string(),
        "3" => "abcdefghijklmnopqrstuvwxyz0123456789".to_string(),
        "4" => "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".to_string(),
        "5" => "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()-_=+[]{}|;:',.<>?/~`".to_string(),
        _ => {
            println!("❌ 无效选项");
            return None;
        }
    };

    println!("\n字符集: {}", charset);
    println!("字符总数: {}", charset.len());

    let (default_min, default_max) = match input.trim() {
        "1" => (4, 6),
        "2" => (3, 5),
        "3" => (3, 5),
        "4" => (3, 4),
        "5" => (3, 4),
        _ => (1, 4),
    };

    print!("\n请输入最小密码长度 (默认 {}): ", default_min);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let min_len = if input.trim().is_empty() {
        default_min
    } else {
        input.trim().parse::<usize>().unwrap_or(default_min).max(1)
    };

    print!("请输入最大密码长度 (默认 {}): ", default_max);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let max_len = if input.trim().is_empty() {
        default_max
    } else {
        input.trim().parse::<usize>().unwrap_or(default_max).max(min_len)
    };

    Some((charset, min_len, max_len))
}

/// 生成单词的所有排列组合
///
/// 示例：words = ["ABC", "XYZ", "123"]
///
/// min_words = 1, max_words = 2:
/// - ABC, XYZ, 123 (单个)
/// - ABCXYZ, ABC123, XYZABC, XYZ123, 123ABC, 123XYZ (两个)
fn generate_word_combinations(words: &[String], min_words: usize, max_words: usize) -> Vec<String> {
    let mut results = Vec::new();

    for num_words in min_words..=max_words.min(words.len()) {
        generate_permutations(words, num_words, &mut Vec::new(), &mut results);
    }

    results
}

/// 递归生成排列（支持重复使用单词）
fn generate_permutations(
    words: &[String],
    remaining: usize,
    current: &mut Vec<usize>,
    results: &mut Vec<String>,
) {
    if remaining == 0 {
        // 拼接当前选中的单词
        let combination: String = current.iter().map(|&i| words[i].as_str()).collect();
        results.push(combination);
        return;
    }

    for i in 0..words.len() {
        current.push(i);
        generate_permutations(words, remaining - 1, current, results);
        current.pop();
    }
}

/// 生成单词组合（不允许重复）
fn generate_word_combinations_no_repeat(words: &[String], min_words: usize, max_words: usize) -> Vec<String> {
    let mut results = Vec::new();

    for num_words in min_words..=max_words.min(words.len()) {
        generate_permutations_no_repeat(words, num_words, &mut Vec::new(), &mut vec![false; words.len()], &mut results);
    }

    results
}

/// 递归生成排列（不允许重复使用单词）
fn generate_permutations_no_repeat(
    words: &[String],
    remaining: usize,
    current: &mut Vec<usize>,
    used: &mut Vec<bool>,
    results: &mut Vec<String>,
) {
    if remaining == 0 {
        let combination: String = current.iter().map(|&i| words[i].as_str()).collect();
        results.push(combination);
        return;
    }

    for i in 0..words.len() {
        if !used[i] {
            current.push(i);
            used[i] = true;
            generate_permutations_no_repeat(words, remaining - 1, current, used, results);
            used[i] = false;
            current.pop();
        }
    }
}

/// 计算组合数量
fn calculate_combination_count(n: usize, min: usize, max: usize, allow_repeat: bool) -> usize {
    let mut total = 0;
    for k in min..=max {
        if allow_repeat {
            // 允许重复：n^k
            total += n.pow(k as u32);
        } else {
            // 不允许重复：P(n, k) = n! / (n-k)!
            if k <= n {
                total += (0..k).fold(1, |acc, i| acc * (n - i));
            }
        }
    }
    total
}

/// 组词攻击（多线程版）
fn word_combination_attack(
    cip_pri_key: Arc<Vec<u8>>,
    words: Vec<String>,
    min_words: usize,
    max_words: usize,
    allow_repeat: bool,
    container_path: &Path, stats: Arc<Stats>,
) -> Option<String> {
    println!("\n🔤 组词攻击模式（多线程）");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("单词列表: {:?}", words);
    println!("单词数量: {}", words.len());
    println!("组合范围: {}-{} 个单词", min_words, max_words);
    println!("允许重复: {}", if allow_repeat { "是" } else { "否" });
    println!("线程数: {} (CPU 核心数)", rayon::current_num_threads());

    // 生成所有组合
    println!("\n生成组合中...");
    let mut combinations = if allow_repeat {
        generate_word_combinations(&words, min_words, max_words)
    } else {
        generate_word_combinations_no_repeat(&words, min_words, max_words)
    };


    let original_count = combinations.len();
    println!("组合总数: {}", original_count);

    // 加载历史并过滤
    let history = load_history(container_path);
    if !history.is_empty() {
        combinations.retain(|p| !history.contains(p));
        let filtered = original_count - combinations.len();

        if filtered > 0 {
            println!("📝 已跳过 {} 个历史密码", filtered);
            println!("   剩余 {} 个新密码", combinations.len());
        }

        if combinations.is_empty() {
            println!("❌ 所有密码都已尝试过");
            return None;
        }
    }

    // 计算预估时间
    let total = combinations.len() as f64;
    let threads = rayon::current_num_threads() as f64;
    let single_thread_time = total * 5.0; // 每次 5 秒
    let multi_thread_time = single_thread_time / threads;

    print!("预估时间（单线程）: ");
    print_time_estimate(single_thread_time);
    print!("预估时间（{}线程）: ", threads as usize);
    print_time_estimate(multi_thread_time);

    // 显示前几个示例
    println!("\n密码组合示例（前 10 个）:");
    for (i, combo) in combinations.iter().take(10).enumerate() {
        println!("  {}. {}", i + 1, combo);
    }
    if combinations.len() > 10 {
        println!("  ... 还有 {} 个组合", combinations.len() - 10);
    }

    println!("\n⚠️  警告: 搜索空间很大可能需要很长时间！");
    print!("确认开始？(y/N): ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    if input.trim().to_lowercase() != "y" {
        println!("已取消");
        return None;
    }

    println!("\n开始尝试...");
    println!("🚀 多线程模式：{} 个并行任务", rayon::current_num_threads());
    println!();

    // 启动进度监控线程
    let stats_clone = Arc::clone(&stats);
    let total_combos = combinations.len();
    let monitor_handle = std::thread::spawn(move || {
        let mut last_attempts = 0;
        while stats_clone.is_running() && !stats_clone.is_found() {
            std::thread::sleep(Duration::from_secs(2));
            if !stats_clone.is_running() || stats_clone.is_found() {
                break;
            }
            let attempts = stats_clone.get_attempts();
            let elapsed = stats_clone.elapsed().as_secs_f64();
            let rate = attempts as f64 / elapsed;
            let speed_per_2s = attempts.saturating_sub(last_attempts);
            last_attempts = attempts;

            // 计算进度
            let progress = (attempts as f64 / total_combos as f64 * 100.0).min(100.0);
            let remaining = total_combos.saturating_sub(attempts as usize);
            let eta_seconds = if rate > 0.0 {
                remaining as f64 / rate
            } else {
                0.0
            };

            // 格式化剩余时间
            let eta_str = if eta_seconds < 60.0 {
                format!("{}秒", eta_seconds as u64)
            } else if eta_seconds < 3600.0 {
                format!("{:.1}分钟", eta_seconds / 60.0)
            } else if eta_seconds < 86400.0 {
                format!("{:.1}小时", eta_seconds / 3600.0)
            } else {
                format!("{:.1}天", eta_seconds / 86400.0)
            };

            // 生成进度条
            let bar_width = 30;
            let filled = (progress / 100.0 * bar_width as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);

            println!(
                "📊 [{}] {:.1}% | {}/{} | 速度 {:.1}/s | 最近2s: {} | ⏱️  剩余 {}",
                bar, progress, attempts, total_combos, rate, speed_per_2s, eta_str
            );
        }
    });

    // 多线程并行搜索
    let result = combinations
        .par_iter()
        .find_map_any(|password| {
            if !stats.is_running() || stats.is_found() {
                return None;
            }

            stats.increment();

            if try_password(&cip_pri_key, password) {
                stats.mark_found();
                stats.stop();
                Some(password.clone())
            } else {
                None
            }
        });

    stats.stop();

    // 保存历史
    save_to_history(container_path, &combinations);
    monitor_handle.join().ok();

    result
}

/// 获取组词配置
fn get_word_combination_config() -> Option<(Vec<String>, usize, usize, bool)> {
    println!("\n🔤 组词攻击配置");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    println!("\n请输入单词片段（用空格分隔）：");
    println!("  示例 1: ABC XYZ 123");
    println!("  示例 2: admin root 2024");
    println!("  示例 3: Zhang Li 1990 Beijing");
    print!("\n单词: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let words: Vec<String> = input
        .trim()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    if words.is_empty() {
        println!("❌ 未输入任何单词");
        return None;
    }

    println!("\n单词列表: {:?}", words);
    println!("单词数量: {}", words.len());

    // 获取组合范围
    print!("\n最少组合几个单词？(默认 1): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let min_words = input.trim().parse::<usize>().unwrap_or(1).max(1);

    print!("最多组合几个单词？(默认 {}): ", words.len());
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let max_words = input
        .trim()
        .parse::<usize>()
        .unwrap_or(words.len())
        .max(min_words)
        .min(words.len());

    // 是否允许重复
    print!("\n允许单词重复使用？(y/N): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let allow_repeat = input.trim().to_lowercase() == "y";

    // 预览组合数量
    let combo_count = calculate_combination_count(words.len(), min_words, max_words, allow_repeat);
    println!("\n将生成约 {} 种密码组合", combo_count);

    Some((words, min_words, max_words, allow_repeat))
}

/// 配置线程数
fn configure_threads() {
    let cpu_count = num_cpus::get();
    let current = rayon::current_num_threads();

    println!("\n⚙️  线程配置");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("CPU 核心数: {}", cpu_count);
    println!("当前线程数: {}", current);
    println!("\n推荐设置:");
    println!("  - 全部核心: {} 线程（最快，但会占满 CPU）", cpu_count);
    println!("  - 一半核心: {} 线程（平衡性能和响应）", cpu_count / 2);
    println!("  - 单线程: 1 线程（用于对比测试）");

    print!("\n请输入线程数 (默认 {}): ", cpu_count);
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();

    let num_threads = if input.trim().is_empty() {
        cpu_count
    } else {
        input.trim().parse::<usize>().unwrap_or(cpu_count).max(1).min(cpu_count * 2)
    };

    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .ok();

    println!("✅ 线程数已设置为: {}", num_threads);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // 显示版本信息
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🔓 Veil 容器暴力破解工具 v{}", VERSION);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // 安全警告
    println!("⚠️  警告: 仅用于测试自己创建的容器!");
    println!();

    // Argon2id 强度提示
    println!("🔐 密钥派生算法: Argon2id");
    println!("   内存消耗: 256 MB");
    println!("   迭代次数: 3");
    println!("   并行度: 4");
    println!();
    println!("💡 提示: Argon2id 是 2015 年密码哈希竞赛获胜者，OWASP/NIST 推荐");
    println!();

    // 获取初始容器路径（命令行参数或交互式输入）
    let mut container_path_str = if args.len() >= 2 {
        args[1].clone()
    } else {
        String::new()
    };

    // 主循环
    loop {
        // 如果没有容器路径，提示输入
        if container_path_str.is_empty() {
            print!("请输入容器文件路径（或输入 'q' 退出）: ");
            io::stdout().flush().ok();

            let mut input = String::new();
            io::stdin().read_line(&mut input).ok();
            let path = input.trim().to_string();

            if path.is_empty() {
                println!("❌ 未输入容器路径");
                continue;
            }

            if path == "q" || path == "Q" {
                println!("\n再见！");
                break;
            }

            container_path_str = path;
        }

        // 检查容器是否存在
        let container_path = Path::new(&container_path_str);
        if !container_path.exists() {
            eprintln!("\n❌ 容器文件不存在: {}", container_path.display());
            eprintln!();
            eprintln!("提示:");
            eprintln!("  1. 检查路径是否正确");
            eprintln!("  2. 使用绝对路径或相对于当前目录的路径");
            eprintln!("  3. 先创建测试容器: cargo run --example create_container --release -- test.veil password");
            eprintln!();

            container_path_str.clear();
            continue;
        }

        // 读取密文私钥
        println!("\n📖 读取容器密文私钥...");
        let cip_pri_key = match File::open(container_path) {
            Ok(mut f) => match format::read_header(&mut f) {
                Ok(header) => {
                    println!("✅ 密文私钥大小: {} 字节", header.cip_pri_key.len());
                    Arc::new(header.cip_pri_key)
                }
                Err(e) => {
                    eprintln!("❌ 读取 Header 失败: {e}");
                    container_path_str.clear();
                    continue;
                }
            },
            Err(e) => {
                eprintln!("❌ 打开容器失败: {e}");
                container_path_str.clear();
                continue;
            }
        };

        println!("✅ 容器: {}", container_path.display());
        println!("✅ CPU 核心数: {}", num_cpus::get());
        println!("✅ 默认线程数: {}", rayon::current_num_threads());

        // 显示历史统计
        show_history_stats(container_path);

        // 攻击菜单循环
        loop {
            let choice = show_menu();

            match choice.as_str() {
                "0" => {
                    println!("\n再见！");
                    return;
                }
                "1" => {
                    if let Some(wordlist_path) = get_wordlist_path() {
                        let stats = Arc::new(Stats::new());
                        let result = dictionary_attack(Arc::clone(&cip_pri_key), Path::new(&wordlist_path), container_path, stats);
                        print_result(result);
                    }
                }
                "2" => {
                    if let Some((charset, min_len, max_len)) = get_custom_charset() {
                        let stats = Arc::new(Stats::new());
                        let result = charset_attack(Arc::clone(&cip_pri_key), &charset, min_len, max_len, stats);
                        print_result(result);
                    }
                }
                "3" => {
                    if let Some((charset, min_len, max_len)) = get_preset_charset() {
                        let stats = Arc::new(Stats::new());
                        let result = charset_attack(Arc::clone(&cip_pri_key), &charset, min_len, max_len, stats);
                        print_result(result);
                    }
                }
                "4" => {
                    if let Some((words, min_words, max_words, allow_repeat)) = get_word_combination_config() {
                        let stats = Arc::new(Stats::new());
                        let result = word_combination_attack(
                            Arc::clone(&cip_pri_key),
                            words,
                            min_words,
                            max_words,
                            allow_repeat,
                            container_path,
                            stats,
                        );
                        print_result(result);
                    }
                }
                "5" => {
                    configure_threads();
                }
                "6" => {
                    println!("\n🔄 切换容器文件");
                    print!("请输入新的容器文件路径: ");
                    io::stdout().flush().ok();

                    let mut input = String::new();
                    io::stdin().read_line(&mut input).ok();
                    let path = input.trim().to_string();

                    if !path.is_empty() {
                        container_path_str = path;
                        break; // 退出攻击菜单，重新加载容器
                    } else {
                        println!("❌ 未输入路径，保持当前容器");
                    }
                }
                "7" => {
                    let stats = Arc::new(Stats::new());
                    let result = retry_history_passwords(Arc::clone(&cip_pri_key), container_path, stats);
                    print_result(result);
                }
                _ => {
                    println!("❌ 无效选项，请重新选择");
                }
            }
        }
    }
}

fn print_result(result: Option<String>) {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    match result {
        Some(password) => {
            println!("✅ 密码已破解!");
            println!();
            println!("🔑 密码: {}", password);
            println!();
            println!("💡 建议: 这个密码太弱了，请使用更强的密码!");
        }
        None => {
            println!("❌ 未能破解密码");
            println!();
            println!("✅ 密码强度较好，或超出当前攻击范围");
        }
    }
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
}
