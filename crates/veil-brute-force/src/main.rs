//! Veil 打包文件密码强度测试工具（交互式多线程版）。
//!
//! **仅供教育目的和测试自己创建的容器使用。**
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
//! - 通过 Arc 共享只读验证快照与原子统计对象
//!
//! 文件作用：本文件是 veil-brute-force 的全部实现——交互式菜单、四种攻击
//! 模式的候选密码生成与并行验证、历史记录去重、线程配置与进度监控。
//! 所属业务模块：Veil 的密码强度测试工具（仅用于用户自测自己创建的 .veil 包）。
//! 核心函数清单：Stats（多线程共享的原子统计）、dictionary_attack（字典攻击）、
//! charset_attack（字符集攻击）、word_combination_attack（组词攻击）、
//! retry_history_passwords（重试历史密码）、configure_threads（线程配置）、
//! main（路径输入与菜单主循环）、print_result（结果汇总）。
//! 依赖能力：veil-core 的 VeilPasswordVerifier（复用加密数据库原生解锁流程来
//! 验证候选密码，不执行解包）、Rayon（并行枚举候选）、num_cpus（核心数探测）、
//! 标准库原子类型（跨线程共享计数与停止标志）。
//! 业务目标：读取包内加密数据库快照后，多线程批量尝试候选密码，命中即原子
//! 置停止标志并汇报；进度监控线程每 2 秒输出速度与预计剩余时间；尝试过的
//! 候选列表追加到用户目录的明文历史文件；提前命中时也可能包含尚未验证的候选，后续过滤不能等同于仅跳过已经验证的口令。

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use veil_core::veil_package::{VeilPasswordVerifier, VeilUnpacker};

/// 版本信息
/// 编译期从 Cargo.toml 注入，启动横幅展示用。
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 历史密码文件目录
/// 位于用户主目录（HOME 或 Windows 的 USERPROFILE）下；存放各容器已尝试
/// 过的候选密码，只用于去重，不参与密码验证。
const HISTORY_DIR: &str = ".veil_history";

/// 把目标包的路径编码成历史文件名，便于复用候选去重记录。
/// 入参：veil_path:&Path，目标包位置；优先 canonicalize，失败保留原路径。
/// 返回值：String，移除 Unix 根前缀或 Windows 盘符后将分隔符改为点并加 .txt。
/// 异常：不返回 Result；路径字符串处理或分配失败可 panic/中止，未校验平台全部非法文件名字符。
/// 注意事项：不是容器稳定 ID：移动路径会换历史；点号和目录分隔符替换可碰撞，Windows 不同盘符也可能共享名称。不同目录的同名包并非必然共享历史。
fn veil_path_to_history_filename(veil_path: &Path) -> String {
    // 优先使用规范化路径，确保同一容器通过不同相对路径访问时共用历史文件。
    let abs_path = std::fs::canonicalize(veil_path).unwrap_or_else(|_| veil_path.to_path_buf());

    let path_str = abs_path.to_string_lossy();

    // 去掉 Unix 根斜杠或 Windows 盘符，避免路径分隔符进入文件名。
    let cleaned = if let Some(stripped) = path_str.strip_prefix('/') {
        stripped
    } else if path_str.len() > 2 && path_str.chars().nth(1) == Some(':') {
        &path_str[3..]
    } else {
        &*path_str
    };

    // 再把剩余目录分隔符统一替换为点号。
    let filename = cleaned.replace(['/', '\\'], ".");

    format!("{}.txt", filename)
}

/// 定位目标包的历史文件并尽力创建存放目录。
/// 入参：veil_path:&Path，传给路径编码函数生成文件名。
/// 返回值：PathBuf，HOME/.veil_history/编码名；HOME 不可读时回退 USERPROFILE，再回退当前目录。
/// 异常：目录创建失败仅警告日志，仍返回预期路径，不返回 Result。
/// 注意事项：不验证环境路径属于用户私有目录，也不显式设置私有权限；读取历史也可能触发目录创建。
fn get_history_file_path(veil_path: &Path) -> std::path::PathBuf {
    // HOME 是 Unix 首选，Windows 则回退到 USERPROFILE。
    let home_dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());

    // 历史目录固定放在用户目录下，文件内容不参与容器身份计算。
    let history_dir = Path::new(&home_dir).join(HISTORY_DIR);

    if let Err(error) = std::fs::create_dir_all(&history_dir) {
        veil_core::logging::failure(
            veil_core::logging::Level::Warn,
            "brute_force",
            "create_history",
            "HISTORY_UNAVAILABLE",
            &error,
            &[],
        );
    }

    let filename = veil_path_to_history_filename(veil_path);
    history_dir.join(filename)
}

/// 读取历史候选去重集合，供下一轮跳过已记录文本。
/// 入参：veil_path:&Path，目标包位置。
/// 返回值：`HashSet<String>`，每项为一行 trim 后的非空候选；重复项合并，文件不存在/打不开为空。
/// 异常：打开失败或首条行读取错误记录日志；map_while 在首次错误处终止，不继续跳过后读取其余行。
/// 注意事项：整份有效前缀载入内存，无数量上限；trim 会改变带首尾空白的原始候选，HashSet 遍历顺序不稳定，记录不证明候选曾实际完成验证。
fn load_history(veil_path: &Path) -> HashSet<String> {
    let history_file = get_history_file_path(veil_path);

    // 首次运行没有历史文件属于正常状态。
    if !history_file.exists() {
        return HashSet::new();
    }

    let file = match File::open(&history_file) {
        Ok(f) => f,
        Err(error) => {
            veil_core::logging::failure(
                veil_core::logging::Level::Warn,
                "brute_force",
                "read_history",
                "HISTORY_UNAVAILABLE",
                &error,
                &[],
            );
            return HashSet::new();
        }
    };

    // 每轮读取一条历史文本并 trim；首次读行错误使 map_while 终止，只保留错误前的记录，不继续后续行。
    BufReader::new(file)
        .lines()
        .map_while(|line| match line {
            Ok(line) => Some(line),
            Err(error) => {
                veil_core::logging::failure(
                    veil_core::logging::Level::Warn,
                    "brute_force",
                    "read_history",
                    "HISTORY_PARTIALLY_READ",
                    &error,
                    &[],
                );
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 将调用方提交的候选数组逐行追加到目标包历史文件。
/// 入参：veil_path:&Path 目标包；passwords:&[String]，每项是一条待记录候选，不在本方法验证是否尝试过。
/// 返回值：()；打开失败直接结束，逐行失败停止，之前成功写入保留。
/// 异常：文件错误记录并打印，不返回 Result；终端错误输出失败可 panic。
/// 注意事项：明文保存候选且无显式私有权限/跨进程锁/同步刷盘；不去重，含换行候选会拆成多条；并行写入可交错，候选记录不等于成功密码。
fn save_to_history(veil_path: &Path, passwords: &[String]) {
    let history_file = get_history_file_path(veil_path);

    // 追加打开，保留已有字节；此函数不确认记录是否已验证，也不去重或限制文件增长。
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_file)
    {
        Ok(f) => f,
        Err(e) => {
            veil_core::logging::failure(
                veil_core::logging::Level::Warn,
                "brute_force",
                "open_history",
                "HISTORY_NOT_SAVED",
                &e,
                &[],
            );
            eprintln!("⚠️  无法保存历史: {}", e);
            return;
        }
    };

    // 单条写入失败后停止，但仍保留此前成功写入的记录。
    for password in passwords {
        if let Err(e) = writeln!(file, "{}", password) {
            veil_core::logging::failure(
                veil_core::logging::Level::Warn,
                "brute_force",
                "write_history",
                "HISTORY_PARTIALLY_SAVED",
                &e,
                &[],
            );
            eprintln!("⚠️  写入历史失败: {}", e);
            break;
        }
    }
}

/// 展示目标包历史去重集合大小和文件位置。
/// 入参：veil_path:&Path 目标包位置。
/// 返回值：()；空集合显示无记录，非空显示去重后的条数与路径。
/// 异常：读取错误被 load_history 折为空集合；println! 失败可 panic。
/// 注意事项：数量是 trim 去空去重后的文本项，不是历史文件原始行数或成功验证数；本方法两次定位目录，可能创建历史目录。
fn show_history_stats(veil_path: &Path) {
    let history = load_history(veil_path);
    let history_file = get_history_file_path(veil_path);

    if history.is_empty() {
        println!("📝 历史记录: 无");
    } else {
        println!("📝 历史记录: {} 个已尝试的密码", history.len());
        println!("   文件位置: {}", history_file.display());
    }
}

/// 在用户确认后，重新并行验证目标包历史文件中可读的候选集合。
/// 入参：verifier（`Arc<VeilPasswordVerifier>`）共享验证器；veil_path（&Path）目标位置用于历史文件定位；stats（`Arc<Stats>`）应为新建运行中状态。
/// 返回值：Some(String) 为任意命中的候选副本；历史为空、用户未确认、停止或未命中均为 None，无法仅凭 None 区分原因。
/// 异常：读取历史错误折为空/部分集合；终端输入错误忽略，线程创建或并行内部 panic 可向上展开，监控 join 错误被忽略。
/// 注意事项：HashSet 转 Vec 后顺序不稳定，find_map_any 不保证先返回文件原顺序的命中项；监控每两秒检查，join 可能等待休眠结束。停止不强制中断在途验证，本函数不新增历史。
fn retry_history_passwords(
    verifier: Arc<VeilPasswordVerifier>,
    veil_path: &Path,
    stats: Arc<Stats>,
) -> Option<String> {
    println!("\n🔄 重试历史密码");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    let history = load_history(veil_path);

    if history.is_empty() {
        println!("❌ 没有历史密码记录");
        return None;
    }

    // 把去重 HashSet 转 Vec 供这里的 par_iter 遍历；顺序不稳定，Rayon 本身也支持其他并行集合形式。
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

    let stats_clone = Arc::clone(&stats);
    // 总候选数拷入闭包，供监控线程计算进度与剩余时间。
    let total_combos = passwords.len();
    // 监控线程只读取原子状态，不参与候选密码尝试。
    let monitor_handle = std::thread::spawn(move || {
        let mut last_attempts = 0;
        // 每 2 秒醒来一次打印一行进度；攻击停止或命中后退出。
        while stats_clone.is_running() && !stats_clone.is_found() {
            std::thread::sleep(Duration::from_secs(2));
            if !stats_clone.is_running() || stats_clone.is_found() {
                break;
            }
            let attempts = stats_clone.get_attempts();
            let elapsed = stats_clone.elapsed().as_secs_f64();
            // 按已启动尝试计数估算平均速度和最近采样差值，不是精确完成量；监控调度间隔也未必恰好两秒。
            let rate = attempts as f64 / elapsed;
            let speed_per_2s = attempts.saturating_sub(last_attempts);
            last_attempts = attempts;

            // 进度百分比（封顶 100）与剩余候选数。
            let progress = (attempts as f64 / total_combos as f64 * 100.0).min(100.0);
            let remaining = total_combos.saturating_sub(attempts as usize);
            // 预计剩余时间 = 剩余候选 / 平均速度；速度为 0 时按 0 秒展示。
            let eta_seconds = if rate > 0.0 {
                remaining as f64 / rate
            } else {
                0.0
            };

            // 按数量级切换剩余时间的展示单位（秒/分钟/小时/天）。
            let eta_str = if eta_seconds < 60.0 {
                format!("{}秒", eta_seconds as u64)
            } else if eta_seconds < 3600.0 {
                format!("{:.1}分钟", eta_seconds / 60.0)
            } else if eta_seconds < 86400.0 {
                format!("{:.1}小时", eta_seconds / 3600.0)
            } else {
                format!("{:.1}天", eta_seconds / 86400.0)
            };

            // 30 格文本进度条：█ 表示已完成的百分比段，░ 表示剩余段。
            let bar_width = 30;
            let filled = (progress / 100.0 * bar_width as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);

            println!(
                "📊 [{}] {:.1}% | {}/{} | 速度 {:.1}/s | 最近2s: {} | ⏱️  剩余 {}",
                bar, progress, attempts, total_combos, rate, speed_per_2s, eta_str
            );
        }
    });

    // 每个并行任务先检查停止标志，找到密码后其余任务尽快退出。
    let result = passwords.par_iter().find_map_any(|password| {
        if !stats.is_running() || stats.is_found() {
            return None;
        }

        stats.increment();

        if try_password(&verifier, password) {
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

/// 多线程攻击共享的计数和状态。
/// 业务定位：一次攻击（一个 Stats 实例）的“仪表盘”——所有工作线程与监控
/// 线程通过它交换尝试次数、运行状态和命中状态，无需互斥锁。
/// 并发模型：四个字段全是原子/不可变时间戳，多个线程可同时读写；
/// 尝试计数用 Relaxed 序即可（只做统计展示，不参与内存同步协议）。
struct Stats {
    /// 已启动的候选验证次数，工作循环在实际验证前递增，因此不等于全部已完成数量。
    attempts: AtomicU64,
    /// 统计对象的创建时刻。
    start_time: Instant,
    /// 攻击是否仍应继续。
    running: AtomicBool,
    /// 是否已经找到密码。
    found: AtomicBool,
}

impl Stats {
    /// 为一次候选验证任务创建可跨线程共享的统计状态。
    /// 入参：无。
    /// 返回值：Stats，次数为零、running=true、found=false，start_time 取当前单调时刻。
    /// 异常：不返回错误。
    /// 注意事项：仅构造状态，不创建工作线程；每轮任务应使用新实例，避免继承已停止标记。
    fn new() -> Self {
        Stats {
            attempts: AtomicU64::new(0),
            start_time: Instant::now(),
            running: AtomicBool::new(true),
            found: AtomicBool::new(false),
        }
    }

    /// 登记一个候选进入验证的尝试，用于速度与进度展示。
    /// 入参：&self，共享统计对象。
    /// 返回值：()，原子 attempts 增加一。
    /// 异常：无 Result；AtomicU64 加法达到最大值会回绕，不作溢出错误返回。
    /// 注意事项：调用处在实际验证前计数，因此表示已启动尝试，不保证都已验证完成；Relaxed 只保证计数自身原子性，不同步其他业务数据。
    fn increment(&self) {
        self.attempts.fetch_add(1, Ordering::Relaxed);
    }

    /// 取得监控时刻观察到的候选尝试计数。
    /// 入参：&self，共享统计对象。
    /// 返回值：u64，单次原子读取值。
    /// 异常：不返回错误。
    /// 注意事项：与 elapsed/running/found 分别读取，不能组成同一瞬间的原子快照；Relaxed 不为其他字段提供先后关系保证。
    fn get_attempts(&self) -> u64 {
        self.attempts.load(Ordering::Relaxed)
    }

    /// 计算本轮统计对象创建后的累计墙钟流逝时间。
    /// 入参：&self，start_time 为不可变单调时钟时刻。
    /// 返回值：Duration，创建至调用的时长，停止后继续增长。
    /// 异常：不返回错误。
    /// 注意事项：不是 CPU 时间，不因 stop 暂停；监控程序需自行决定何时取最终耗时。
    fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// 请求候选工作循环不再启动后续验证。
    /// 入参：&self，共享统计对象。
    /// 返回值：()，将 running 原子写为 false。
    /// 异常：不返回错误。
    /// 注意事项：协作式停止：在途密钥验证不会被强行中断，线程何时观察到标记取决于检查时机；不等待 join。
    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// 让工作线程检查当前是否仍允许开始候选验证。
    /// 入参：&self，共享统计对象。
    /// 返回值：bool，running 的本次原子读取值。
    /// 异常：不返回错误。
    /// 注意事项：true 不证明没有其他线程随后调用 stop，读取与启动验证之间仍有竞争窗口。
    fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// 记录至少一个候选已通过验证，提示其他线程尽快停止。
    /// 入参：&self，共享统计对象。
    /// 返回值：()，found 原子写为 true。
    /// 异常：不返回错误。
    /// 注意事项：不保存命中的密码、不同步结果存储，也不自动写 running=false；调用方另行 stop 并通过并行任务返回结果。
    fn mark_found(&self) {
        self.found.store(true, Ordering::Relaxed);
    }

    /// 检查是否已有线程标记候选验证成功。
    /// 入参：&self，共享统计对象。
    /// 返回值：bool，本次读取 found 标记。
    /// 异常：不返回错误。
    /// 注意事项：不是获取命中密码的接口，false 与下一时刻命中可并存；不替代等待任务完成。
    fn is_found(&self) -> bool {
        self.found.load(Ordering::Relaxed)
    }
}

/// 用包内加密数据库的原生解锁流程检验一条候选口令。
/// 入参：verifier:&VeilPasswordVerifier 预先从测试目标构造且可共享的验证器；password:&str 候选 UTF-8 口令。
/// 返回值：bool，通过验证为 true，其余 false，失败原因不在此保留。
/// 异常：不返回 Result；内部 panic 不在此捕获。
/// 注意事项：同步且可能消耗显著计算资源；本方法不更新 Stats、不保存历史或清除调用方口令副本，具体派生和解锁参数由目标数据库决定。
fn try_password(verifier: &VeilPasswordVerifier, password: &str) -> bool {
    verifier.matches_password(password)
}

/// 从字典读入候选并按历史去重，再并行尝试目标包的口令。
/// 入参：verifier（`Arc<VeilPasswordVerifier>`）共享验证器；wordlist_path（&Path）UTF-8 逐行词表；veil_path（&Path）历史定位路径；stats（`Arc<Stats>`）本轮新建统计。
/// 返回值：命中为 Some(String)，打不开词表、候选被历史全部过滤、停止或未命中为 None，不返回详细错误状态。
/// 异常：词表打开失败记录错误并返回；逐行读取首个错误使 map_while 停止且不报告，线程创建/输出/并行验证可 panic；监控 join 错误忽略。
/// 注意事项：候选完整载入内存，trim 后去空但不在本轮去重。结束时保存整个过滤后候选数组，包括命中或停止后未真正验证的项，后续可能因此跳过未试候选；不是可靠的已完成验证审计。
fn dictionary_attack(
    verifier: Arc<VeilPasswordVerifier>,
    wordlist_path: &Path,
    veil_path: &Path,
    stats: Arc<Stats>,
) -> Option<String> {
    println!("\n🔍 字典攻击模式（多线程）");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("单词表: {}", wordlist_path.display());
    println!("线程数: {} (CPU 核心数)", rayon::current_num_threads());

    let file = match File::open(wordlist_path) {
        Ok(f) => f,
        Err(e) => {
            veil_core::logging::error_chain("brute_force", "read_wordlist", &e);
            eprintln!("❌ 无法打开单词表: {e}");
            return None;
        }
    };

    // 本实现选择先完整收集候选 Vec 再 par_iter，不是流式词表消费；首条行读取错误会停止后续收集。
    let mut passwords: Vec<String> = BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    println!("密码总数: {}", passwords.len());
    let original_count = passwords.len();
    println!("密码总数: {}", original_count);

    // 历史记录过滤：已试过的密码不再重复消耗计算资源。
    let history = load_history(veil_path);
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
    // 词典攻击的监控线程只打印进度，不持有候选密码所有权。
    let monitor_handle = std::thread::spawn(move || {
        let mut last_attempts = 0;
        // 每 2 秒打印一次累计尝试次数、平均速度与最近 2 秒完成量。
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

    // 找到结果后通过共享原子状态通知其他 Rayon 任务停止。
    let result = passwords.par_iter().find_map_any(|password| {
        // 协作式停止检查：一旦命中或外部停止，本任务立即放弃剩余候选。
        if !stats.is_running() || stats.is_found() {
            return None;
        }

        stats.increment();

        if try_password(&verifier, password) {
            stats.mark_found();
            stats.stop();
            Some(password.clone())
        } else {
            None
        }
    });

    stats.stop();

    // 保存全部候选而非实际验证完成列表，提前命中后尚未验证的项也可能被下一轮过滤。
    save_to_history(veil_path, &passwords);
    monitor_handle.join().ok();

    result
}

/// 枚举给定字符表的定长候选，用于一批并行验证。
/// 入参：charset:&[char] 按选择顺序排列的字符；length:usize 为 Unicode 字符数量，不是 UTF-8 字节数。
/// 返回值：`Vec<String>`，按字符表索引里程表顺序排列；length=0 返回唯一空串。
/// 异常：charset 为空且 length>0 会在 `charset[0]` 索引 panic，并非返回空集合；组合数大时可能耗尽内存。
/// 注意事项：不排序或去重 charset，重复字符会产生重复候选；一次构造全部 n^length 个字符串，调用方需控制规模。
fn generate_all_combinations(charset: &[char], length: usize) -> Vec<String> {
    // 空密码长度定义为唯一空字符串，便于统一搜索循环。
    if length == 0 {
        return vec![String::new()];
    }

    // indices 是 charset 的基数计数器，每一位对应生成字符串的一位。
    let mut results = Vec::new();
    let mut indices = vec![0usize; length];

    loop {
        // 把当前计数器的每一位映射成字符，得到一条候选密码。
        let combination: String = indices.iter().map(|&i| charset[i]).collect();
        results.push(combination);

        // 从最低位开始递增；溢出时归零并向高位进位。
        // 相当于一个“length 位的 charset.len() 进制里程表”：最低位 +1，
        // 加满进位到更高位；最高位也溢出时说明全部组合已枚举完毕。
        let mut pos = length - 1;
        loop {
            indices[pos] += 1;
            if indices[pos] < charset.len() {
                break;
            }
            indices[pos] = 0;
            if pos == 0 {
                return results;
            }
            pos -= 1;
        }
    }
}

/// 经终端确认后按字符长度逐批生成候选，并用共享验证器并行检验。
/// 入参：verifier（`Arc<VeilPasswordVerifier>`）共享验证器；charset（&str）字符表，按 char 而非字节计长度；min_len/max_len（usize）闭区间；stats（`Arc<Stats>`）本轮运行中统计。
/// 返回值：任意候选命中为 Some(String)，取消、停止或耗尽为 None；不保存此模式历史。
/// 异常：输入/flush 错误被忽略；空字符表正长度会在生成器 panic，线程创建/终端输出/分配也可能失败，监控 join 错误忽略。
/// 注意事项：搜索量用 checked_pow 和饱和加法预估，指数 usize 转 u32 可能截断；总数转 usize 在窄平台可能截断。每个长度仍完整驻留内存，不是内存硬上限；生成期间不轮询停止，验证在途也不强制中断。
fn charset_attack(
    verifier: Arc<VeilPasswordVerifier>,
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

    // 搜索空间按字符数的幂逐长度累加（c^min + c^(min+1) + ... + c^max）。
    let mut total_combinations = 0u64;
    for len in min_len..=max_len {
        // 搜索空间按长度逐项累加，溢出时饱和为 u64 上界。
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

        // 单线程吞吐按每秒 0.2 次估算，多线程部分只做理想线性折算。
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

    let stats_clone = Arc::clone(&stats);
    let total_combos = total_combinations as usize;
    // 字符集攻击按长度分批生成候选，监控线程复用同一组原子统计。
    // 监控循环体与 retry_history_passwords 相同：每 2 秒输出进度条、速度与
    // 预计剩余时间（逐行含义见该函数内的注释）。
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

            let progress = (attempts as f64 / total_combos as f64 * 100.0).min(100.0);
            let remaining = total_combos.saturating_sub(attempts as usize);
            let eta_seconds = if rate > 0.0 {
                remaining as f64 / rate
            } else {
                0.0
            };

            let eta_str = if eta_seconds < 60.0 {
                format!("{}秒", eta_seconds as u64)
            } else if eta_seconds < 3600.0 {
                format!("{:.1}分钟", eta_seconds / 60.0)
            } else if eta_seconds < 86400.0 {
                format!("{:.1}小时", eta_seconds / 3600.0)
            } else {
                format!("{:.1}天", eta_seconds / 86400.0)
            };

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

    // 按长度从小到大遍历，每轮生成该长度的完整候选；这只是固定搜索顺序，不依据真实口令分布预测。
    for length in min_len..=max_len {
        // 每一轮只保留当前长度的组合，降低峰值内存占用。
        if !stats.is_running() || stats.is_found() {
            break;
        }

        println!("尝试 {} 位密码...", length);

        let combinations = generate_all_combinations(&chars, length);
        println!("  组合数: {}", combinations.len());

        // 本批次并行验证；与字典攻击相同的协作式停止协议。
        let found = combinations.par_iter().find_map_any(|password| {
            if !stats.is_running() || stats.is_found() {
                return None;
            }

            stats.increment();

            if try_password(&verifier, password) {
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

/// 把候选搜索的估算秒数转换为可读时间单位并打印。
/// 入参：seconds（f64）预期为非负有限秒数；本方法未校验。
/// 返回值：()，依数量级显示秒、分钟、小时、天或年。
/// 异常：println! 输出失败可 panic，不返回 I/O 错误。
/// 注意事项：只是展示估算，不参与调度或超时控制；负数、NaN、Infinity 不会被主动拒绝，结果不应当作完成时间保证。
fn print_time_estimate(seconds: f64) {
    // 根据数量级切换单位，让超大搜索空间的估算仍保持可读。
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

/// 显示任务模式与配置/切换/退出菜单，读取一次用户选项。
/// 入参：无；从 stdin 读一行。
/// 返回值：String，首尾 trim 后的选项，未验证编号；EOF/读取失败通常返回空串。
/// 异常：read_line/flush 结果忽略，println! 失败可能 panic。
/// 注意事项：菜单是七个功能项，并非七种验证算法；无效/空输入由主循环处理，没有在此自动退出。
fn show_menu() -> String {
    // 菜单只返回用户原始选择，具体参数收集在对应攻击分支中完成。
    println!("\n╔════════════════════════════════════════════╗");
    println!("║  Veil 容器暴力破解工具                 ║");
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

/// 收集字典模式所需的词表位置。
/// 入参：无；从 stdin 读取一行路径文本。
/// 返回值：Some(String) 为 trim 后非空路径，空输入或无内容读取失败为 None。
/// 异常：不返回 I/O 错误，输出失败可能 panic。
/// 注意事项：不检查文件存在或可读性，不自动去除 shell 引号或展开 ~；文件读取由后续字典函数执行。
fn get_wordlist_path() -> Option<String> {
    // 空输入表示取消本次攻击，而不是使用隐式默认路径。
    println!("\n请输入单词表文件路径:");
    println!("  示例: common_passwords.txt");
    println!("  示例: crates/veil-brute-force/common_passwords.txt");
    print!("  路径: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let path = input.trim().to_string();

    if path.is_empty() { None } else { Some(path) }
}

/// 让用户组合字符组并给出候选长度范围。
/// 入参：无；依次读取字符组选项、可选自定义文本和长度。
/// 返回值：Some((String,usize,usize)) 为排序去重后的字符表、最小和最大 char 数；空字符集为 None。
/// 异常：输入错误忽略，长度解析失败回退 1/4，输出或分配可 panic。
/// 注意事项：最小至少 1，最大至少最小，没有上限。自定义字符 trim 丢弃首尾空白；展示 charset.len() 为 UTF-8 字节数，非 ASCII 时不等于实际 char 数。
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
    let selections: Vec<&str> = input.split_whitespace().collect();

    let mut charset = String::new();

    // 每个选项把对应字符组追加到同一字符集，5 允许用户直接补充字符。
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

    // 排序并去重，保证相同字符不会让搜索空间重复计算。
    let mut chars: Vec<char> = charset.chars().collect();
    chars.sort_unstable();
    chars.dedup();
    charset = chars.into_iter().collect();

    println!("\n最终字符集: {}", charset);
    println!("字符总数: {}", charset.len());

    print!("\n请输入最小密码长度 (默认 1): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    // 最小长度至少为 1，无效输入回退到默认值 1。
    let min_len = input.trim().parse::<usize>().unwrap_or(1).max(1);

    print!("请输入最大密码长度 (默认 4): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    // 最大长度不得小于最小长度，从而保证搜索范围始终有效。
    let max_len = input.trim().parse::<usize>().unwrap_or(4).max(min_len);

    Some((charset, min_len, max_len))
}

/// 选择一个内置字符表并读取本轮最小、最大长度。
/// 入参：无；从 stdin 读预设编号及两次长度输入。
/// 返回值：预设 1..5 有效时 Some((字符表,最小长度,最大长度))，其他编号 None；空长度采用该预设默认值。
/// 异常：输入/flush 错误忽略，解析失败回退预设值；终端输出可 panic。
/// 注意事项：显式最大值会夹到不小于最小值，但最大输入为空直接用 default_max，若用户最小值更大，可能返回反向空区间；没有搜索规模上限。
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

    // 预设编号同时决定字符集以及后续长度输入的默认值。
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
    // 空输入采用当前预设建议值，否则修正到至少 1。
    let min_len = if input.trim().is_empty() {
        default_min
    } else {
        input.trim().parse::<usize>().unwrap_or(default_min).max(1)
    };

    print!("请输入最大密码长度 (默认 {}): ", default_max);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    // 仅非空输入分支夹到 min_len；空输入直接保留 default_max，用户调高最小值时可能出现 max_len<min_len。
    let max_len = if input.trim().is_empty() {
        default_max
    } else {
        input
            .trim()
            .parse::<usize>()
            .unwrap_or(default_max)
            .max(min_len)
    };

    Some((charset, min_len, max_len))
}

/// 按指定片段数量枚举允许重复选择的拼接候选。
/// 入参：words:&[String] 单词片段；min_words/max_words:usize 闭区间，实际最大值截到 words.len()。
/// 返回值：`Vec<String>`，先按片段数量升序，再按输入下标顺序深度优先拼接，无分隔符；区间为空则空数组。
/// 异常：无 Result，递归过深或结果过大可栈溢出/内存耗尽。
/// 注意事项：虽然允许重复，实际最大深度仍限制为片段总数；不是候选总数被限制为 words.len()。不同片段组合可能生成同一字符串，不去重。
fn generate_word_combinations(words: &[String], min_words: usize, max_words: usize) -> Vec<String> {
    let mut results = Vec::new();

    // 外层遍历单词数量，内层递归枚举该数量的所有排列。
    for num_words in min_words..=max_words.min(words.len()) {
        generate_permutations(words, num_words, &mut Vec::new(), &mut results);
    }

    results
}

/// 递归填满剩余片段位置，把每条完整选择路径转为候选字符串。
/// 入参：words:&[String] 片段表；remaining:usize 还需填的位置；current:&mut `Vec<usize>` 当前片段下标栈；results:&mut `Vec<String>` 候选收集器。
/// 返回值：()，每个叶子把 current 顺序拼接并追加 results，正常返回恢复 current 的进入状态。
/// 异常：current 含非法下标会 panic，递归/分配无上限可能耗尽资源。
/// 注意事项：每轮选择一个片段索引，push→递归→pop 回溯，同一索引可多次选择；panic 时不保证栈已恢复。
fn generate_permutations(
    words: &[String],
    remaining: usize,
    current: &mut Vec<usize>,
    results: &mut Vec<String>,
) {
    if remaining == 0 {
        // current 保存单词下标，到达目标深度后按顺序拼接为候选密码。
        let combination: String = current.iter().map(|&i| words[i].as_str()).collect();
        results.push(combination);
        return;
    }

    for i in 0..words.len() {
        // 允许重复时每个位置都可再次选择任意单词。
        current.push(i);
        generate_permutations(words, remaining - 1, current, results);
        current.pop();
    }
}

/// 按片段数量枚举同一输入索引不重复使用的候选。
/// 入参：words:&[String] 输入片段；min_words/max_words:usize 闭区间，最大截到片段总数。
/// 返回值：`Vec<String>`，不同长度依次收集，长度为零时可生成空串；无有效长度则为空。
/// 异常：递归过深、分配失败等不转 Result。
/// 注意事项：每个长度新建 used，按索引而非文本去重：输入里相同文本的不同下标仍能同时使用并产生重复候选。
fn generate_word_combinations_no_repeat(
    words: &[String],
    min_words: usize,
    max_words: usize,
) -> Vec<String> {
    let mut results = Vec::new();

    // 不允许重复时，每个长度使用独立的 used 标记数组。
    for num_words in min_words..=max_words.min(words.len()) {
        generate_permutations_no_repeat(
            words,
            num_words,
            &mut Vec::new(),
            &mut vec![false; words.len()],
            &mut results,
        );
    }

    results
}

/// 用已用标记回溯枚举一个不重复索引的片段排列。
/// 入参：words:&[String]；remaining:usize 待填数量；current:&mut `Vec<usize>` 已选索引；used:&mut `Vec<bool>` 长度须与 words 一致；results:&mut `Vec<String>` 输出候选。
/// 返回值：()，叶子追加字符串，正常回溯时撤销本层 used 和 current。
/// 异常：used 太短或 current 索引非法可 panic；栈和内存耗尽不受控。
/// 注意事项：每轮对应一个输入片段，未用才标记并递归；无足够未用索引时自然无叶子结果，不报告业务错误。
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
            // 先标记当前选择，递归返回后再撤销，形成标准回溯。
            current.push(i);
            used[i] = true;
            generate_permutations_no_repeat(words, remaining - 1, current, used, results);
            used[i] = false;
            current.pop();
        }
    }
}

/// 预估允许或禁止重复选择时指定片段数量的索引排列总数。
/// 入参：n:usize 片段数；min/max:usize 闭区间；allow_repeat:bool，true 累加 n^k，false 累加 P(n,k) 且 k>n 不计。
/// 返回值：usize，仅为索引组合数，不消除相同文本拼接造成的重复。
/// 异常：普通加法、乘法和 pow 可在 Debug 溢出 panic，Release 可回绕；k 转 u32 也可能截断。
/// 注意事项：没有饱和或上限保护；允许重复时估算 max 不像生成器截到 n，展示数量可能与实际生成数量不一致，不可用于安全分配保证。
fn calculate_combination_count(n: usize, min: usize, max: usize, allow_repeat: bool) -> usize {
    let mut total = 0;
    // 对每个允许长度分别累加，结果只用于向用户预览规模。
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

/// 根据片段拼接规则生成候选，经确认后并行验证并保存历史。
/// 入参：verifier（`Arc<VeilPasswordVerifier>`）共享验证器；words（`Vec<String>`）按值接管片段表；min_words/max_words（usize）片段数量闭区间；allow_repeat（bool）是否重复选同一索引；veil_path（&Path）历史定位目标；stats（`Arc<Stats>`）本轮统计。
/// 返回值：命中为 Some(String)，取消、停止、历史过滤后无候选或未命中为 None。
/// 异常：递归生成、内存分配、线程创建和终端输出可 panic/中止；输入错误忽略，历史读写错误被底层吞掉并记录。
/// 注意事项：完整生成发生在用户确认之前，规模过大时不能靠取消确认避免生成成本；先按历史过滤，再展示最多十条候选明文。结束保存全部剩余候选，含提前停止后未验证项；固定五秒估计不是目标数据库性能实测。
fn word_combination_attack(
    verifier: Arc<VeilPasswordVerifier>,
    words: Vec<String>,
    min_words: usize,
    max_words: usize,
    allow_repeat: bool,
    veil_path: &Path,
    stats: Arc<Stats>,
) -> Option<String> {
    println!("\n🔤 组词攻击模式（多线程）");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("单词列表: {:?}", words);
    println!("单词数量: {}", words.len());
    println!("组合范围: {}-{} 个单词", min_words, max_words);
    println!("允许重复: {}", if allow_repeat { "是" } else { "否" });
    println!("线程数: {} (CPU 核心数)", rayon::current_num_threads());

    println!("\n生成组合中...");
    // 按是否允许重复选择对应的生成器，一次性构造全部候选。
    let mut combinations = if allow_repeat {
        generate_word_combinations(&words, min_words, max_words)
    } else {
        generate_word_combinations_no_repeat(&words, min_words, max_words)
    };

    let original_count = combinations.len();
    println!("组合总数: {}", original_count);

    // 与字典攻击一致：过滤历史记录中已尝试过的候选。
    let history = load_history(veil_path);
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

    // 固定每候选五秒作示意估算，不测量当前机器或目标数据库派生参数，多线程仅理想线性折算。
    let total = combinations.len() as f64;
    let threads = rayon::current_num_threads() as f64;
    let single_thread_time = total * 5.0; // 每次 5 秒
    let multi_thread_time = single_thread_time / threads;

    print!("预估时间（单线程）: ");
    print_time_estimate(single_thread_time);
    print!("预估时间（{}线程）: ", threads as usize);
    print_time_estimate(multi_thread_time);

    // 展示前 10 条候选示例，让用户确认拼接顺序符合预期。
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

    let stats_clone = Arc::clone(&stats);
    let total_combos = combinations.len();
    // 组词攻击仍需一次性构造候选列表，监控线程只负责进度与 ETA。
    // 监控循环体与 retry_history_passwords 相同：每 2 秒输出进度条、速度与
    // 预计剩余时间（逐行含义见该函数内的注释）。
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

            let progress = (attempts as f64 / total_combos as f64 * 100.0).min(100.0);
            let remaining = total_combos.saturating_sub(attempts as usize);
            let eta_seconds = if rate > 0.0 {
                remaining as f64 / rate
            } else {
                0.0
            };

            let eta_str = if eta_seconds < 60.0 {
                format!("{}秒", eta_seconds as u64)
            } else if eta_seconds < 3600.0 {
                format!("{:.1}分钟", eta_seconds / 60.0)
            } else if eta_seconds < 86400.0 {
                format!("{:.1}小时", eta_seconds / 3600.0)
            } else {
                format!("{:.1}天", eta_seconds / 86400.0)
            };

            let bar_width = 30;
            let filled = (progress / 100.0 * bar_width as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);

            println!(
                "📊 [{}] {:.1}% | {}/{} | 速度 {:.1}/s | 最近2s: {} | ⏱️  剩余 {}",
                bar, progress, attempts, total_combos, rate, speed_per_2s, eta_str
            );
        }
    });

    // Rayon 在首个命中后返回，停止标志让其他已启动任务跳过剩余候选。
    let result = combinations.par_iter().find_map_any(|password| {
        if !stats.is_running() || stats.is_found() {
            return None;
        }

        stats.increment();

        if try_password(&verifier, password) {
            stats.mark_found();
            stats.stop();
            Some(password.clone())
        } else {
            None
        }
    });

    stats.stop();

    // 与字典攻击一致：本轮全部候选写入历史，命中结果随之上报。
    save_to_history(veil_path, &combinations);
    monitor_handle.join().ok();

    result
}

/// 读取单词片段、拼接深度和是否重复使用索引的交互配置。
/// 入参：无；从 stdin 顺序读取多行。
/// 返回值：Some((`Vec<String>`,usize,usize,bool)) 为按空白切分的片段、最小数量、最大数量和重复开关；无片段为 None。
/// 异常：读入/flush 错误被忽略；输出、组合数量溢出或分配可 panic，未返回 Result。
/// 注意事项：不去重片段，也不支持片段内空白；最大值先至少 min 再封顶 words.len()，若 min 超过片段数会产生反向空区间。允许重复也受相同上限限制；数量预估不构成内存配额。
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
    let words: Vec<String> = input.split_whitespace().map(|s| s.to_string()).collect();

    // 空单词列表无法生成候选，直接取消本次攻击配置。
    if words.is_empty() {
        println!("❌ 未输入任何单词");
        return None;
    }

    println!("\n单词列表: {:?}", words);
    println!("单词数量: {}", words.len());

    print!("\n最少组合几个单词？(默认 1): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    // 最少单词数至少为 1，解析失败时使用默认值。
    let min_words = input.trim().parse::<usize>().unwrap_or(1).max(1);

    print!("最多组合几个单词？(默认 {}): ", words.len());
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    // 先抬到 min_words 再封顶片段数；min_words 大于片段数时仍可能得到 max_words<min_words。
    let max_words = input
        .trim()
        .parse::<usize>()
        .unwrap_or(words.len())
        .max(min_words)
        .min(words.len());

    print!("\n允许单词重复使用？(y/N): ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    // 只有明确输入 y 才允许重复，回车默认关闭。
    let allow_repeat = input.trim().to_lowercase() == "y";

    // 预览值可能溢出，仅用于提示用户搜索规模。
    let combo_count = calculate_combination_count(words.len(), min_words, max_words, allow_repeat);
    println!("\n将生成约 {} 种密码组合", combo_count);

    Some((words, min_words, max_words, allow_repeat))
}

/// 读取用户希望的 Rayon 线程数，并尝试配置进程全局线程池。
/// 入参：无；读取 CPU 可用核心数和 stdin 文本。
/// 返回值：()，打印请求的线程数，不返回实际构建成功状态。
/// 异常：输入/flush 及 build_global 错误被忽略，输出失败可 panic。
/// 注意事项：先调用 current_num_threads 本身可能初始化全局池，因此随后 build_global 通常无法重建；“已设置”提示不证明生效。非空数值夹在 1..=核心数两倍，空/非法值用核心数，不保证全部核心一定最快。
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

    // 空输入使用 CPU 核心数，显式输入限制在 1 到核心数两倍之间。
    let num_threads = if input.trim().is_empty() {
        cpu_count
    } else {
        input
            .trim()
            .parse::<usize>()
            .unwrap_or(cpu_count)
            .max(1)
            .min(cpu_count * 2)
    };

    // build_global 只能成功一次；已初始化时保留现有线程池并继续运行。
    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .ok();

    println!("✅ 线程数已设置为: {}", num_threads);
}

/// 加载目标包的口令验证快照，并循环收集验证模式与配置，输出本轮结果。
/// 入参：无 Rust 参数；`argv[1]` 可为初始包路径，其余参数忽略；未提供或加载失败时从 stdin 读取。
/// 返回值：()；外层路径输入 q/Q 后 break 并请求日志刷新，内层菜单 0 直接 return，跳过末尾显式刷新。
/// 异常：目标不存在和包读取错误记录/提示后重试，不通过非零进程码报告；stdin/flush 错误忽略，输出、线程或内部算法 panic 未捕获。
/// 注意事项：EOF 空输入会反复提示而非自动退出。验证器按每次外层加载缓存并 Arc 共享；菜单每轮使用新 Stats，但候选与命中字符串不安全清零。横幅派生参数为固定文案，不是从目标数据库读取的实际参数。
fn main() {
    veil_core::logging::initialize();
    // 进程参数集合，每项是一个 Unicode 参数；只使用第一个路径参数，没有 clap 式帮助或选项解析。
    let args: Vec<String> = std::env::args().collect();

    // 启动横幅与授权边界提示：本工具只用于测试自己创建的容器。
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🔓 Veil 容器暴力破解工具 v{}", VERSION);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    println!("⚠️  警告: 仅用于测试自己创建的容器!");
    println!();

    // 下列参数是固定说明，不从已选包读取；真正验证使用数据库原生解锁流程，不能把横幅视为该目标配置。
    println!("🔐 密钥派生算法: Argon2id");
    println!("   内存消耗: 256 MB");
    println!("   迭代次数: 3");
    println!("   并行度: 4");
    println!();
    println!("💡 提示: Argon2id 是 2015 年密码哈希竞赛获胜者，OWASP/NIST 推荐");
    println!();

    // 命令行路径优先，否则进入交互式输入循环。
    let mut veil_path_str = if args.len() >= 2 {
        args[1].clone()
    } else {
        String::new()
    };

    // 外层循环：负责拿到（或重新输入）一个存在的容器路径。
    loop {
        if veil_path_str.is_empty() {
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

            veil_path_str = path;
        }

        let veil_path = Path::new(&veil_path_str);
        if !veil_path.exists() {
            veil_core::logging::record(
                veil_core::logging::Level::Info,
                "brute_force",
                "open_container",
                "FILE_NOT_FOUND",
            );
            eprintln!("\n❌ 容器文件不存在: {}", veil_path.display());
            eprintln!();
            eprintln!("提示:");
            eprintln!("  1. 检查路径是否正确");
            eprintln!("  2. 使用绝对路径或相对于当前目录的路径");
            eprintln!(
                "  3. 先创建自己的 Veil，再用 veil pack <Veil> --output test.veil 生成测试文件"
            );
            eprintln!();

            veil_path_str.clear();
            continue;
        }

        println!("\n📖 读取打包文件的加密元数据...");
        // 元数据只读取一次；并行候选共享验证器，不执行解包或读取文件内容。
        let verifier = match VeilUnpacker::new(veil_path).read_password_verifier() {
            Ok(verifier) => {
                println!("✅ 已加载加密元数据");
                // Arc 让多个 Rayon 工作线程安全地共享同一验证器。
                Arc::new(verifier)
            }
            Err(error) => {
                veil_core::logging::error_chain("brute_force", "read_package", &error);
                eprintln!("❌ 读取打包文件失败: {error}");
                eprintln!("提示: 仅支持 veil pack 生成的 .veil 文件");
                veil_path_str.clear();
                continue;
            }
        };

        println!("✅ 容器: {}", veil_path.display());
        println!("✅ CPU 核心数: {}", num_cpus::get());
        // 查询全局线程数会触发 Rayon 默认线程池初始化，后续菜单配置并不能可靠重建它。
        println!("✅ 默认线程数: {}", rayon::current_num_threads());

        show_history_stats(veil_path);

        // 当前容器的攻击菜单循环；切换容器时跳出并重新读取包内元数据。
        loop {
            let choice = show_menu();

            match choice.as_str() {
                // 直接返回整个 main；不会到达外层循环后的显式 logging::flush。
                "0" => {
                    println!("\n再见！");
                    return;
                }
                // 1：从用户给出的词表读取候选密码。
                "1" => {
                    if let Some(wordlist_path) = get_wordlist_path() {
                        let stats = Arc::new(Stats::new());
                        let result = dictionary_attack(
                            Arc::clone(&verifier),
                            Path::new(&wordlist_path),
                            veil_path,
                            stats,
                        );
                        print_result(result);
                    }
                }
                // 2：使用交互式构造的自定义字符集。
                "2" => {
                    if let Some((charset, min_len, max_len)) = get_custom_charset() {
                        let stats = Arc::new(Stats::new());
                        let result = charset_attack(
                            Arc::clone(&verifier),
                            &charset,
                            min_len,
                            max_len,
                            stats,
                        );
                        print_result(result);
                    }
                }
                // 3：使用内置预设字符集。
                "3" => {
                    if let Some((charset, min_len, max_len)) = get_preset_charset() {
                        let stats = Arc::new(Stats::new());
                        let result = charset_attack(
                            Arc::clone(&verifier),
                            &charset,
                            min_len,
                            max_len,
                            stats,
                        );
                        print_result(result);
                    }
                }
                // 4：按单词排列生成组合密码。
                "4" => {
                    if let Some((words, min_words, max_words, allow_repeat)) =
                        get_word_combination_config()
                    {
                        let stats = Arc::new(Stats::new());
                        let result = word_combination_attack(
                            Arc::clone(&verifier),
                            words,
                            min_words,
                            max_words,
                            allow_repeat,
                            veil_path,
                            stats,
                        );
                        print_result(result);
                    }
                }
                // 5：调整 Rayon 全局线程池。
                "5" => {
                    configure_threads();
                }
                // 6：清空当前容器状态，跳出菜单后重新读取包内元数据。
                "6" => {
                    println!("\n🔄 切换容器文件");
                    print!("请输入新的容器文件路径: ");
                    io::stdout().flush().ok();

                    let mut input = String::new();
                    io::stdin().read_line(&mut input).ok();
                    let path = input.trim().to_string();

                    if !path.is_empty() {
                        veil_path_str = path;
                        break;
                    } else {
                        println!("❌ 未输入路径，保持当前容器");
                    }
                }
                // 7：重新尝试历史文件中已记录过的密码。
                "7" => {
                    let stats = Arc::new(Stats::new());
                    let result = retry_history_passwords(Arc::clone(&verifier), veil_path, stats);
                    print_result(result);
                }
                _ => {
                    println!("❌ 无效选项，请重新选择");
                }
            }
        }
    }
    // 仅从外层 break 退出的路径到达此处；尽力刷新且忽略错误，不保证磁盘物理同步。
    let _ = veil_core::logging::flush();
}

/// 将本轮候选验证结果展示为统一的用户反馈。
/// 入参：result（`Option<String>`）取得结果所有权，Some 是命中口令副本，None 也可能来自取消、读入失败或未命中。
/// 返回值：()，把口令或未命中提示直接打印到标准输出。
/// 异常：println! 输出失败可能 panic，不返回 I/O 错误。
/// 注意事项：Some 的明文会进入终端/重定向输出，字符串释放未安全零化；None 不能证明口令强度，也不能确认全部候选都实际尝试。
fn print_result(result: Option<String>) {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    // Some 是验证命中，None 汇合未命中、取消和部分输入错误，不足以判定口令强度。
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
