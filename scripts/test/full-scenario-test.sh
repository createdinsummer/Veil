#!/bin/sh
#
# Veil 全功能黑盒场景测试脚本。
#
# 默认用阶段 HOME 隔离服务数据，报告与运行数据路径可由环境变量覆盖；外置盘测试还会使用指定磁盘位置。
# 工具链缓存继续使用原用户目录，不能把整个脚本理解为完全不访问用户目录。
#
# 常用方式：
#   sh scripts/test/full-scenario-test.sh
#   sh scripts/test/full-scenario-test.sh --quick
#   sh scripts/test/full-scenario-test.sh --external /Volumes/MyDisk
#   sh scripts/test/full-scenario-test.sh --non-interactive --no-build
#
# 自动测试统一使用密码 "1"；交互测试会要求用户直接在终端输入密码。

# 未定义变量导致失败；未启用 set -e，测试函数需要自行读取退出码，不能假定命令失败即停止整个脚本。
set -u

# 不继承开发终端中的密码、语言和 KDF 控制变量。
unset VEIL_PASSWORD VEIL_NEW_PASSWORD VEIL_LANG VEIL_HINTS VEIL_TEST_KDF 2>/dev/null || true

# 将 rustup/cargo 缓存固定在调用者真实用户目录。脚本稍后会切换 HOME 做隔离；
# 若不显式保留这些路径，临时目录下的辅助程序将找不到默认 Rust 工具链。
ORIGINAL_HOME=${HOME:-}
if [ -n "$ORIGINAL_HOME" ]; then
    RUSTUP_HOME=${RUSTUP_HOME:-"$ORIGINAL_HOME/.rustup"}
    CARGO_HOME=${CARGO_HOME:-"$ORIGINAL_HOME/.cargo"}
    export RUSTUP_HOME CARGO_HOME
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
# 当前实现只取脚本目录的上一层；脚本位于 scripts/test 时此值为 scripts，而非仓库根目录。
# 下方二进制和 Cargo 路径按此值拼接；此处仅说明现状，不修改路径逻辑。
PROJECT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

# 默认时间戳精确到秒；RUN_ROOT 额外包含进程号，REPORT_ROOT 没有进程号，并发运行可能共享报告目录。
RUN_STAMP=$(date '+%Y%m%d-%H%M%S')
REPORT_ROOT=${VEIL_REPORT_ROOT:-"$PROJECT_ROOT/test-reports/veil-$RUN_STAMP"}
RUN_ROOT=${VEIL_TEST_ROOT:-"/tmp/veil-full-test-$RUN_STAMP-$$"}

# 在绝对路径写入链接和配置前先规范化根目录。macOS 上 /tmp 会解析到
# /private/tmp，混用两种写法会导致合法的链接缓存无法命中。
mkdir -p "$RUN_ROOT" "$REPORT_ROOT"
RUN_ROOT=$(CDPATH= cd -- "$RUN_ROOT" && pwd -P)
REPORT_ROOT=$(CDPATH= cd -- "$REPORT_ROOT" && pwd -P)

QUICK=0
NON_INTERACTIVE=0
NO_BUILD=0
RELEASE_FULL=0
KEEP_DATA=0
RUN_EXTERNAL=0
RUN_TTY=0
RUN_STRESS=0
RUN_BRUTE=0
EXTERNAL_DIR=${VEIL_EXTERNAL_VOLUME:-}
EXTERNAL_CYCLES=${VEIL_EXTERNAL_CYCLES:-3}

DEBUG_BIN="$PROJECT_ROOT/target/debug/veil"
RELEASE_BIN="$PROJECT_ROOT/target/release/veil"
BRUTE_BIN="$PROJECT_ROOT/target/debug/veil-brute-force"

# 场景计数与断言计数相互独立：PASS/FAIL 主要按命令退出码，CHECK/ASSERT_FAIL 按额外业务断言；SKIP 为跳过，NOTE 为观察说明。
PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
NOTE_COUNT=0
CHECK_COUNT=0
ASSERT_FAIL_COUNT=0

# 当前阶段标签及其 HOME/工作路径由 phase 替换；CASE_OUT/ERR 保存最近 run_case 的报告位置，不是全部用例集合。
CURRENT_PHASE=setup
PHASE_HOME="$RUN_ROOT/home"
PHASE_WORK="$RUN_ROOT/work"
CASE_OUT="$RUN_ROOT/.case.out"
CASE_ERR="$RUN_ROOT/.case.err"
# 两个辅助程序路径用于摘要及 JSONC 读写；首次调用按需编译，缓存文件在 RUN_ROOT 内。
BLAKE3_HELPER_BIN=""
JSONC_HELPER_BIN="$RUN_ROOT/jsonc-helper"

# 功能：输出一行测试说明，不改变计数。
# 入参：任意数量文本参数，$* 合并为一行。
# 返回：printf 的退出状态。
# 异常：Shell 不抛出语言异常；命令失败按返回状态或上述规则处理。
# 注意：不脱敏文本，也不额外附加阶段或用例标识。
log_line() {
    printf '%s\n' "$*"
}

# 功能：屏蔽以 VEIL_PASSWORD= 或 VEIL_NEW_PASSWORD= 开头的单个命令参数。
# 入参：$1 原始参数字符串；返回：标准输出中的替换文本，其他形式原样输出。
# 异常：不主动退出，返回 printf 状态；注意：不覆盖命令行 -p 密码、路径或其他秘密，不能视为通用脱敏器。
redact_arg() {
    case "$1" in
        VEIL_PASSWORD=*|VEIL_NEW_PASSWORD=*)
            printf '%s=***' "${1%%=*}"
            ;;
        *)
            printf '%s' "$1"
            ;;
    esac
}

# 功能：用分隔线突出报告中的场景分组。
# 入参：任意文本参数，合并为标题。
# 返回：最后 printf 的退出状态。
# 异常：Shell 不抛出语言异常；命令失败按返回状态或上述规则处理。
# 注意：只输出标题，不切换 phase 或重置计数。
section() {
    printf '\n'
    printf '%s\n' "============================================================"
    printf '%s\n' "$*"
    printf '%s\n' "============================================================"
}

# 功能：登记一条不属于失败的观察说明。
# 入参：任意文本参数，合并为说明文案。
# 返回：增加 NOTE_COUNT 并输出编号，返回 printf 状态。
# 异常：Shell 不抛出语言异常；命令失败按返回状态或上述规则处理。
# 注意：不增加 CHECK_COUNT、PASS_COUNT 或失败数，观察本身不能证明用例通过。
note() {
    NOTE_COUNT=$((NOTE_COUNT + 1))
    printf '[说明 %03d] %s\n' "$NOTE_COUNT" "$*"
}

# 功能：登记一个未执行或无法确认的测试项。
# 入参：$1 用例 ID；$2 跳过原因字符串。
# 返回：增加 SKIP_COUNT 并输出原因，返回 printf 状态。
# 异常：Shell 不抛出语言异常；命令失败按返回状态或上述规则处理。
# 注意：一次调用只计一个跳过项，不代表整个分组中每个子用例都逐项计数。
skip_case() {
    SKIP_COUNT=$((SKIP_COUNT + 1))
    printf '[跳过] %-10s %s\n' "$1" "$2"
}

# 功能：为一组测试切换阶段专属 HOME、工作目录和临时目录。
# 入参：$1 阶段名，直接参与路径拼接，必须是脚本内部可信标签。
# 返回：最后 printf 的状态；改变 CURRENT_PHASE、PHASE_HOME、PHASE_WORK、HOME、TMPDIR 及当前目录。
# 异常：cd 失败显式退出 1，mkdir 失败没有单独检查；注意：同名阶段复用目录，不自动清空旧文件，也不覆盖所有可能继承的服务配置变量。
phase() {
    CURRENT_PHASE=$1
    PHASE_HOME="$RUN_ROOT/home-$1"
    PHASE_WORK="$RUN_ROOT/work-$1"
    mkdir -p "$PHASE_HOME" "$PHASE_WORK"
    HOME=$PHASE_HOME
    TMPDIR="$RUN_ROOT/tmp-$1"
    mkdir -p "$TMPDIR"
    export HOME TMPDIR
    cd "$PHASE_WORK" || exit 1
    printf '\n[阶段] %s\n' "$CURRENT_PHASE"
    printf '  隔离 HOME: %s\n' "$PHASE_HOME"
    printf '  工作目录: %s\n' "$PHASE_WORK"
}

# 功能：为产物观察输出路径、长度和摘要。
# 入参：$1 待检查路径。
# 返回：标准输出目录、文件或不存在说明；状态通常来自最后 printf。
# 异常：Shell 不抛出语言异常；命令失败按返回状态或上述规则处理。
# 注意：test -e 追随链接，悬空链接显示不存在；非目录未进一步确认普通文件。摘要标签固定 sha256，但 hash_file 可能回退 cksum。
describe_file() {
    path=$1
    if [ -e "$path" ]; then
        if [ -d "$path" ]; then
            printf '目录 %s\n' "$path"
        else
            bytes=$(wc -c < "$path" 2>/dev/null | tr -d ' ')
            hash=$(hash_file "$path" 2>/dev/null || printf '不可用')
            printf '文件 %s 字节=%s sha256=%s\n' "$path" "$bytes" "$hash"
        fi
    else
        printf '不存在 %s\n' "$path"
    fi
}

# 功能：为前后内容比较提供摘要文本，优先 SHA-256，缺工具时退回 cksum。
# 入参：$1 文件路径；返回：标准输出摘要，cksum 回退为校验值:字节数，不是 SHA-256。
# 异常：无语言异常；注意：管道未启用 pipefail，底层读取失败可能被末尾 awk 的成功状态掩盖，调用方不能只靠状态证明读取成功。
hash_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        cksum "$1" | awk '{print $1 ":" $2}'
    fi
}

# 功能：使用 b3sum 或临时编译的 Rust 程序计算文件 BLAKE3。
# 入参：$1 文件路径；读取项目 Debug 依赖 rlib，必要时设置 BLAKE3_HELPER_BIN。
# 返回：标准输出十六进制摘要；依赖缺失或编译失败返回 1，否则透传辅助命令状态。
# 异常：Shell 不抛出语言异常；命令失败按返回状态或上述规则处理。
# 注意：b3sum 管道状态可能掩盖读取失败；Rust 备用程序将整文件读入内存，非流式，读取失败会 panic。命令替换子 Shell 中的缓存变量修改不回传父进程。
blake3_hex() {
    file=$1
    if command -v b3sum >/dev/null 2>&1; then
        b3sum "$file" | awk '{ print $1 }'
        return
    fi

    if [ -z "$BLAKE3_HELPER_BIN" ] || [ ! -x "$BLAKE3_HELPER_BIN" ]; then
        blake3_rlib=$(ls -t "$PROJECT_ROOT"/target/debug/deps/libblake3-*.rlib 2>/dev/null | head -n 1)
        if [ -z "$blake3_rlib" ]; then
            return 1
        fi
        helper_source="$RUN_ROOT/blake3-helper.rs"
        BLAKE3_HELPER_BIN="$RUN_ROOT/blake3-helper"
        cat > "$helper_source" <<'EOF'
/// 为文件内容一致性检查计算 BLAKE3 摘要。
/// 入参：无 Rust 参数；进程 argv[1] 是文件路径，要求除程序名外恰好一个参数。
/// 返回：正常结束返回 ()，并在标准输出打印十六进制摘要；参数数量不符直接退出 2。
/// 异常：文件读取失败由 expect 触发 panic；不返回 Result，也不把错误转换为测试断言。
/// 注意：一次读入完整文件，内存用量随文件大小增长；命令行参数必须能表示为 Unicode 字符串。
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        std::process::exit(2);
    }
    // data 保存整个样本的原始字节，不做文本解码；摘要反映字节内容，不包含路径或权限。
    let data = std::fs::read(&args[1]).expect("read input");
    println!("{}", blake3::hash(&data).to_hex());
}
EOF
        rustc --edition=2024 "$helper_source" \
            -L "dependency=$PROJECT_ROOT/target/debug/deps" \
            --extern "blake3=$blake3_rlib" \
            -o "$BLAKE3_HELPER_BIN" >/dev/null 2>&1 || return 1
    fi

    "$BLAKE3_HELPER_BIN" "$file"
}

# 功能：读取样本文件的字节数，用于容量或完整性比较。
# 入参：$1 文件路径；返回：标准输出中的十进制字节数字符串，状态来自末尾 tr。
# 异常：读取失败由命令报告；注意：无 pipefail，wc 失败可能被 tr 的成功状态掩盖。
file_size() {
    wc -c < "$1" | tr -d ' '
}

# 功能：兼容 macOS 和 GNU stat 读取 Unix 权限数字。
# 入参：$1 路径。
# 返回：标准输出权限文本；优先 BSD stat，失败后尝试 GNU stat，返回最后执行命令状态。
# 异常：Shell 不抛出语言异常；命令失败按返回状态或上述规则处理。
# 注意：不检查所有者或 ACL；不同 stat 实现的行为由系统决定。
file_mode() {
    stat -f '%Lp' "$1" 2>/dev/null || stat -c '%a' "$1" 2>/dev/null
}

# 功能：按需编译内嵌 Rust 辅助程序，再读取或改写 JSONC 测试夹具。
# 入参：全部参数原样转发；get/set 使用文件和 JSON Pointer（以 / 分段的字段定位表达式），work 使用文件、工作区名和根目录，resolve 解析链接。
# 返回：辅助程序标准输出和退出状态；找不到依赖库或编译失败返回 1。
# 异常：Shell 不抛异常，Rust 程序错误以非零状态报告；注意：依赖 target/debug/deps 中最新修改的 rlib，可能不匹配当前编译器或构建配置。
# set/work 会把文件改写为格式化 JSON，丢失原 JSONC 注释；resolve 还读取当前隔离 HOME 下的配置。
jsonc_tool() {
    if [ ! -x "$JSONC_HELPER_BIN" ]; then
        jsonc_core_rlib=$(ls -t "$PROJECT_ROOT"/target/debug/deps/libveil_core-*.rlib 2>/dev/null | head -n 1)
        jsonc_serde_rlib=$(ls -t "$PROJECT_ROOT"/target/debug/deps/libserde_json-*.rlib 2>/dev/null | head -n 1)
        [ -n "$jsonc_core_rlib" ] && [ -n "$jsonc_serde_rlib" ] || return 1
        jsonc_helper_source="$RUN_ROOT/jsonc-helper.rs"
        cat > "$jsonc_helper_source" <<'EOF'
use serde_json::Value;
use std::error::Error;
use std::{env, fs, path::PathBuf};
use veil_core::config::GlobalConfig;

/// 为场景测试读取 JSONC 字段、修改字符串、登记工作区或解析链接位置。
/// 入参：无 Rust 参数；argv 依次为动作、文件、字段指针或工作区名，以及 set/work 所需的新值/根目录。
/// 返回：成功为 Ok(())；get/resolve 打印结果，set/work 覆盖输入文件后结束。
/// 异常：参数不足、字段不存在、未知动作、配置加载/登记、JSONC 解析、编码或文件读写失败返回 Err。
/// 注意：先统一读取解析 document，resolve 也要求至少四个 argv 项；修改写回格式化 JSON，不保留注释或排版，也没有原子替换。
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        return Err("用法：jsonc-helper <get|set|work> <文件> <路径或工作区名> [值]".into());
    }
    let path = &args[2];
    // 通用 JSON 值树保存文件中的对象、数组及叶子字段；所有动作先解析，因此 resolve 也不能跳过输入文件语法检查。
    let mut document: Value = veil_core::jsonc::from_str(&fs::read_to_string(path)?)?;
    match args[1].as_str() {
        // JSON Pointer 定位树中的一项字段；字符串直接输出内容，其余类型以 JSON 表示输出。
        "get" => {
            let value = document.pointer(&args[3]).ok_or("JSONC 字段不存在")?;
            match value {
                Value::String(value) => println!("{value}"),
                _ => println!("{value}"),
            }
            return Ok(());
        }
        // 重新按链接类型加载文件，并结合当前 HOME 配置解析密文目录；不修改 document 后的输入文件。
        "resolve" => {
            let link = veil_core::link::VeilLink::load(std::path::Path::new(path))?;
            println!("{}", GlobalConfig::load()?.resolve_link_veil_dir(&link, std::path::Path::new(path))?.display());
            return Ok(());
        }
        // 仅替换已有字段为字符串，不能自动创建不存在的路径，也不把新值解析为数字或对象。
        "set" => {
            let value = args.get(4).ok_or("缺少字段的新值")?;
            *document.pointer_mut(&args[3]).ok_or("JSONC 字段不存在")? = Value::String(value.clone());
        }
        // 按真实 GlobalConfig 规则登记自定义工作区，再把类型化配置转换回通用值树。
        "work" => {
            let root = args.get(4).ok_or("缺少工作区根目录")?;
            let mut config = GlobalConfig::load_from(std::path::Path::new(path))?;
            config.register_custom_work(args[3].clone(), PathBuf::from(root), None)?;
            document = serde_json::to_value(config)?;
        }
        _ => return Err("未知 JSONC 操作".into()),
    }
    // 只有 set/work 会到达这里；覆盖原文件，无临时文件回滚，写入失败可留下部分内容。
    fs::write(path, serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}
EOF
        rustc --edition=2024 "$jsonc_helper_source" \
            -L "dependency=$PROJECT_ROOT/target/debug/deps" \
            --extern "veil_core=$jsonc_core_rlib" \
            --extern "serde_json=$jsonc_serde_rlib" \
            -o "$JSONC_HELPER_BIN" || return 1
    fi
    "$JSONC_HELPER_BIN" "$@"
}

# 功能：按可执行性和部分源文件修改时间判断二进制是否需要重建。
# 入参：$1 二进制路径；返回：退出码 0 表示缺失或检测到更新，非零表示没有发现更新。
# 异常：find 错误被重定向且管道无 pipefail；注意：只检查列出的扩展名和路径，遗漏文件或构建参数变化不会触发，不等于完整依赖指纹。
binary_is_stale() {
    binary=$1
    [ -x "$binary" ] || return 0
    find "$PROJECT_ROOT/crates" "$PROJECT_ROOT/Cargo.toml" "$PROJECT_ROOT/Cargo.lock" \
        -type f \( -name '*.rs' -o -name '*.jsonc' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
        -newer "$binary" -print -quit 2>/dev/null | grep -q .
}

# 功能：统计指定目录第一层匹配 *.enc 且不是 AppleDouble（._ 前缀）的普通文件。
# 入参：$1 目录路径；返回：标准输出计数，状态来自 tr。
# 异常：find 错误不单独返回；注意：直接数磁盘文件，不读取数据库登记，也不验证密文是否被引用或有效。
count_enc_files() {
    find "$1" -maxdepth 1 -type f -name '*.enc' ! -name '._*' | wc -l | tr -d ' '
}

# 功能：累加目录树所有普通文件的磁盘内容长度，作为大小比较基准。
# 入参：$1 根目录；返回：标准输出总字节数，不区分明文、密文、数据库等文件类型。
# 异常：管道错误可能被 awk 掩盖；注意：按换行分隔路径，文件名含换行时不可靠，awk 大数求和也不保证整数精确。
work_file_bytes() {
    find "$1" -type f -print | while IFS= read -r file; do
        file_size "$file"
    done | awk '{ total += $1 } END { print total + 0 }'
}

# 功能：逐条读取包中的数据段，确认每段在工作目录能找到同长度同摘要的密文文件。
# 入参：$1 包路径；$2 密文目录；$3 预期检查条数（非负整数）；返回：全部匹配为 0，发现不匹配返回 1 并打印条目名。
# 异常：未集中检查 dd/od 失败，损坏包可能导致算术错误；注意：按本机 od 字节序解释长度，不校验包签名、末尾数据、条目名映射或一对一匹配。
package_entries_match_work() {
    package=$1
    work=$2
    expected_count=$3
    metadata_size=$(dd if="$package" bs=1 skip=14 count=4 2>/dev/null | od -An -tu4 | tr -d ' \n')
    position=$((22 + metadata_size))
    index=0

    # 每轮一条包记录：读取名称和长度、抽取数据、与目录内候选密文比较，再把偏移移到下一条。
    while [ "$index" -lt "$expected_count" ]; do
        name_len=$(dd if="$package" bs=1 skip="$position" count=2 2>/dev/null | od -An -tu2 | tr -d ' \n')
        name_start=$((position + 2))
        entry_name=$(dd if="$package" bs=1 skip="$name_start" count="$name_len" 2>/dev/null)
        size_position=$((name_start + name_len))
        data_size=$(dd if="$package" bs=1 skip="$size_position" count=8 2>/dev/null | od -An -tu8 | tr -d ' \n')
        data_start=$((size_position + 8))
        extracted="$RUN_ROOT/package-entry-$index.bin"
        dd if="$package" of="$extracted" bs=1 skip="$data_start" count="$data_size" 2>/dev/null
        extracted_hash=$(hash_file "$extracted")

        matched=0
        # 每轮一个顶层 .enc 候选；只要任一文件长度与摘要相同就接受，不标记候选已被其他记录使用。
        for encrypted in "$work"/*.enc; do
            [ -f "$encrypted" ] || continue
            if [ "$(file_size "$encrypted")" = "$data_size" ] && [ "$(hash_file "$encrypted")" = "$extracted_hash" ]; then
                matched=1
                break
            fi
        done
        if [ "$matched" -ne 1 ]; then
            printf '包条目与工作区密文不匹配: %s (%s 字节)\n' "$entry_name" "$data_size" >&2
            return 1
        fi

        position=$((data_start + data_size))
        index=$((index + 1))
    done
    return 0
}

PRE_HASH=""
POST_HASH=""

# 功能：按路径排序输出普通文件快照，供命令前后对比。
# 入参：$1 根目录；返回：标准输出根目录说明及每项相对路径、字节数、摘要，以制表符分隔，不包含权限。
# 异常：根目录不存在时输出说明后正常返回；注意：不自行写文件，调用方负责重定向；换行或制表符文件名可能破坏记录边界。
snapshot_work() {
    root=$1
    if [ ! -d "$root" ]; then
        printf '工作区不存在=%s\n' "$root"
        return
    fi
    printf '工作区=%s\n' "$root"
    find "$root" -type f -print | LC_ALL=C sort | while IFS= read -r file; do
        rel=${file#"$root"/}
        bytes=$(wc -c < "$file" 2>/dev/null | tr -d ' ')
        hash=$(hash_file "$file" 2>/dev/null || printf '不可用')
        printf '%s\t%s\t%s\n' "$rel" "$bytes" "$hash"
    done
}

# 功能：将当前目录内容快照写入本次运行文件并打印，供后续人工或自动比较。
# 入参：$1 快照标签；$2 目录路径；返回：cat 状态，不自动更新断言计数。
# 异常：写入或读取失败可返回非零；注意：文件名由阶段和标签组成，重复标签覆盖旧快照，标签必须是可信路径片段。
capture_snapshot() {
    label=$1
    path=$2
    out="$RUN_ROOT/snapshot-$CURRENT_PHASE-$label.txt"
    snapshot_work "$path" > "$out"
    printf '[快照] %s\n' "$out"
    cat "$out"
}

# 功能：执行一条检查命令并累计断言结果。
# 入参：$1 断言 ID；$2 业务说明；其余参数为命令和各个独立实参，不是布尔字符串。
# 返回：命令退出 0 则返回 0，其他状态返回 1；始终增加 CHECK_COUNT，失败再增加 ASSERT_FAIL_COUNT。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：命令本身可能有副作用；变量没有 local 隔离，调用者不应依赖同名全局变量不变。
assert_true() {
    id=$1
    description=$2
    shift 2
    if "$@"; then
        printf '[断言通过] %-10s %s\n' "$id" "$description"
        CHECK_COUNT=$((CHECK_COUNT + 1))
        return 0
    fi
    printf '[断言失败] %-10s %s\n' "$id" "$description"
    CHECK_COUNT=$((CHECK_COUNT + 1))
    ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    return 1
}

# 功能：检查预期路径存在。
# 入参：$1 ID；$2 说明；$3 路径。
# 返回：透传 assert_true 的 0/1 和计数更新。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：test -e 不限普通文件，目录也通过；悬空符号链接按不存在处理。
assert_file_exists() {
    assert_true "$1" "$2" test -e "$3"
}

# 功能：检查预期路径按 test -e 判断不存在。
# 入参：$1 ID；$2 说明；$3 路径。
# 返回：透传 assert_true 的 0/1 和计数更新。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：悬空符号链接也会通过，不能据此断言所有目录项都已删除。
assert_file_missing() {
    assert_true "$1" "$2" test ! -e "$3"
}

# 功能：按字节比较两个业务样本。
# 入参：$1 ID；$2 说明；$3 第一个文件；$4 第二个文件。
# 返回：cmp 成功返回 0，否则返回 1 并累计断言失败。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：不区分内容不同与读取失败，两种情况都视为断言失败。
assert_file_equals() {
    assert_true "$1" "$2" cmp -s "$3" "$4"
}

# 功能：比较期望值与实际值的文本表示。
# 入参：$1 ID；$2 说明；$3 期望字符串；$4 实际字符串。
# 返回：完全相同返回 0，否则 1；增加检查数，失败增加断言失败数。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：不是数值比较，01 与 1 不同；失败报告会输出原始值，调用方不要传入真实秘密。
assert_values_equal() {
    id=$1
    description=$2
    expected=$3
    actual=$4
    if [ "$expected" = "$actual" ]; then
        printf '[断言通过] %-10s %s（%s）\n' "$id" "$description" "$actual"
        CHECK_COUNT=$((CHECK_COUNT + 1))
        return 0
    fi
    printf '[断言失败] %-10s %s（预期 %s，实际 %s）\n' "$id" "$description" "$expected" "$actual"
    CHECK_COUNT=$((CHECK_COUNT + 1))
    ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    return 1
}

# 功能：验证两个业务标识或结果文本不同。
# 入参：$1 ID；$2 说明；$3 第一值；$4 第二值。
# 返回：不同为 0，相同为 1；更新检查和失败计数。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：只比较字符串，不证明两项资源在其他层面隔离；报告可能显示原始值。
assert_values_not_equal() {
    id=$1
    description=$2
    first=$3
    second=$4
    if [ "$first" != "$second" ]; then
        printf '[断言通过] %-10s %s\n' "$id" "$description"
        CHECK_COUNT=$((CHECK_COUNT + 1))
        return 0
    fi
    printf '[断言失败] %-10s %s（两个值不应相同：%s）\n' "$id" "$description" "$first"
    CHECK_COUNT=$((CHECK_COUNT + 1))
    ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    return 1
}

# 功能：检查指定文本未在一组报告文件中出现。
# 入参：$1 ID；$2 说明；$3 禁止出现的固定字符串；其余参数为文件路径集合，每轮检查一份报告。
# 返回：任何普通文件匹配即返回 1，否则返回 0；每次调用只累计一条断言。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：非普通文件、缺失文件和 grep 读取错误都会被当成未匹配，因此通过不证明每个文件都已成功读取。
assert_not_contains() {
    id=$1
    description=$2
    pattern=$3
    shift 3
    for file in "$@"; do
        if [ -f "$file" ] && grep -Fq -- "$pattern" "$file" 2>/dev/null; then
            printf '[断言失败] %-10s %s（%s 包含禁止内容）\n' "$id" "$description" "$file"
            CHECK_COUNT=$((CHECK_COUNT + 1))
            ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
            return 1
        fi
    done
    printf '[断言通过] %-10s %s\n' "$id" "$description"
    CHECK_COUNT=$((CHECK_COUNT + 1))
    return 0
}

# 功能：验证目标当前权限文本恰好为 600。
# 入参：$1 ID；$2 说明；$3 文件或目录路径。
# 返回：模式等于 600 返回 0，否则返回 1 并登记失败。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：不是通用私有目录检查（目录通常需要 700），不校验所有者、ACL 或是否为普通文件。
assert_private_mode() {
    id=$1
    description=$2
    file=$3
    mode=$(file_mode "$file")
    if [ "$mode" = "600" ]; then
        printf '[断言通过] %-10s %s（600）\n' "$id" "$description"
        CHECK_COUNT=$((CHECK_COUNT + 1))
        return 0
    fi
    printf '[断言失败] %-10s %s（预期 600，实际 %s）\n' "$id" "$description" "${mode:-未知}"
    CHECK_COUNT=$((CHECK_COUNT + 1))
    ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    return 1
}

# 功能：检查报告文件中包含预期固定文本。
# 入参：$1 ID；$2 说明；$3 固定字符串；$4 待读取文件路径。
# 返回：grep 匹配返回 0，未匹配或读取失败返回 1，累计一次断言。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：使用固定文本而非正则；错误输出被隐藏，不能区分缺内容和文件无法读取。
assert_contains() {
    id=$1
    description=$2
    pattern=$3
    file=$4
    if grep -Fq -- "$pattern" "$file" 2>/dev/null; then
        printf '[断言通过] %-10s %s\n' "$id" "$description"
        CHECK_COUNT=$((CHECK_COUNT + 1))
        return 0
    fi
    printf '[断言失败] %-10s %s（缺少：%s）\n' "$id" "$description" "$pattern"
    CHECK_COUNT=$((CHECK_COUNT + 1))
    ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    return 1
}

# 功能：执行一个命令，保存完整输出并按退出码登记场景结果。
# 入参：$1 用例 ID；$2 期望整数退出码；$3 说明；其余为命令及独立实参。
# 返回：匹配增加 PASS_COUNT，不匹配增加 FAIL_COUNT；两种情况均返回 0，让后续场景继续。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：不自动校验业务内容或设置超时；ID 直接组成文件名，同 ID 覆盖输出。仅命令参数局部脱敏，stdout/stderr 原样报告，不能用于真实秘密。
run_case() {
    id=$1
    expected=$2
    description=$3
    shift 3

    CASE_OUT="$RUN_ROOT/cases/$id.stdout"
    CASE_ERR="$RUN_ROOT/cases/$id.stderr"
    mkdir -p "$RUN_ROOT/cases"

    printf '\n[用例 %s] %s\n' "$id" "$description"
    printf '[命令 ]'
    for arg in "$@"; do
        printf ' '
        redact_arg "$arg"
    done
    printf '\n'

    "$@" > "$CASE_OUT" 2> "$CASE_ERR"
    actual=$?

    printf '[退出码] 预期=%s 实际=%s\n' "$expected" "$actual"
    printf '%s\n' '--- 标准输出 ---'
    cat "$CASE_OUT"
    printf '%s\n' '--- 标准错误 ---'
    cat "$CASE_ERR"
    printf '%s\n' '--- 用例结束 ---'

    if [ "$actual" -eq "$expected" ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        printf '[通过] %s\n' "$id"
        return 0
    fi

    FAIL_COUNT=$((FAIL_COUNT + 1))
    printf '[失败] %s 预期退出码 %s，实际 %s\n' "$id" "$expected" "$actual"
    return 0
}

# 功能：把一段可信 Shell 命令文本交给统一场景执行器。
# 入参：$1 ID；$2 期望退出码；$3 说明；$4 完整 Shell 代码文本。
# 返回：由 run_case 更新场景计数，返回其状态（通常为 0）。
# 异常：Shell 不抛出语言异常，命令错误按上述状态规则处理。
# 注意：通过 /bin/sh -c 解释，包含重定向、管道和替换等真实代码能力；只传脚本内构造的测试命令，不是交互终端密码测试。
run_shell_case() {
    id=$1
    expected=$2
    description=$3
    command_text=$4
    run_case "$id" "$expected" "$description" /bin/sh -c "$command_text"
}

# 功能：并发启动两段 Shell 命令并记录双方输出和退出码。
# 入参：$1 ID；$2 说明；$3 第一段 Shell 代码；$4 第二段 Shell 代码。
# 返回：等待两个进程后无条件增加 PASS_COUNT，最后 printf 状态通常为 0，不按 rc1/rc2 判定成败。
# 异常：无语言异常，命令或退出状态按上述规则处理。
# 注意：两条命令共享当前阶段目录和环境，无超时或自动取消；打印的命令文本未经 redact_arg，调用方不能放入真实秘密。
run_pair() {
    id=$1
    description=$2
    first=$3
    second=$4
    first_out="$RUN_ROOT/cases/$id.first.stdout"
    first_err="$RUN_ROOT/cases/$id.first.stderr"
    second_out="$RUN_ROOT/cases/$id.second.stdout"
    second_err="$RUN_ROOT/cases/$id.second.stderr"
    mkdir -p "$RUN_ROOT/cases"

    printf '\n[用例 %s] %s\n' "$id" "$description"
    printf '[命令1] %s\n' "$first"
    printf '[命令2] %s\n' "$second"

    # 先启动双方并保存各自 PID；wait 顺序不决定命令执行顺序，两者都可能在等待前已完成。
    /bin/sh -c "$first" > "$first_out" 2> "$first_err" &
    pid1=$!
    /bin/sh -c "$second" > "$second_out" 2> "$second_err" &
    pid2=$!
    wait "$pid1"
    rc1=$?
    wait "$pid2"
    rc2=$?

    printf '[退出码1] %s\n' "$rc1"
    printf '%s\n' '--- 标准输出 1 ---'
    cat "$first_out"
    printf '%s\n' '--- 标准错误 1 ---'
    cat "$first_err"
    printf '[退出码2] %s\n' "$rc2"
    printf '%s\n' '--- 标准输出 2 ---'
    cat "$second_out"
    printf '%s\n' '--- 标准错误 2 ---'
    cat "$second_err"
    printf '%s\n' '--- 用例结束 ---'

    PASS_COUNT=$((PASS_COUNT + 1))
    printf '[观察] %s 已完成，测试脚本正常结束\n' "$id"
}

# 功能：直接连接当前终端执行交互命令并按退出码登记结果。
# 入参：$1 ID；$2 期望退出码整数；$3 说明；其余为命令和参数。
# 返回：匹配增加 PASS_COUNT，否则 FAIL_COUNT；末尾打印状态通常为 0，不返回被测命令退出码。
# 异常：无语言异常，命令或退出状态按上述规则处理。
# 注意：虽设置 CASE_OUT/CASE_ERR 路径，但没有把交互输出重定向到它们；不能把这些变量当作本次交互已采集的报告。
run_interactive_case() {
    id=$1
    expected=$2
    description=$3
    shift 3

    CASE_OUT="$RUN_ROOT/cases/$id.stdout"
    CASE_ERR="$RUN_ROOT/cases/$id.stderr"
    mkdir -p "$RUN_ROOT/cases"

    printf '\n[用例 %s] %s\n' "$id" "$description"
    printf '[交互] 请按提示操作，具体输入以用例描述为准。\n'
    "$@"
    actual=$?
    printf '[退出码] 预期=%s 实际=%s\n' "$expected" "$actual"
    if [ "$actual" -eq "$expected" ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        printf '[通过] %s\n' "$id"
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        printf '[失败] %s 预期退出码 %s，实际 %s\n' "$id" "$expected" "$actual"
    fi
}

# 功能：运行人工输入场景，并将不符合预期的结果视为不可确认观察。
# 入参：$1 ID；$2 期望退出码；$3 说明；其余为命令和参数。
# 返回：匹配增加 PASS_COUNT，其他结果增加 SKIP_COUNT，不增加 FAIL_COUNT；最后 printf 状态通常为 0。
# 异常：无语言异常，命令或退出状态按上述规则处理。
# 注意：仍按退出码比较，但无法确认隐藏密码实际输入内容，因此失败不作为自动失败；不采集独立 stdout/stderr 文件。
run_interactive_observation() {
    id=$1
    expected=$2
    description=$3
    shift 3

    printf '\n[用例 %s] %s\n' "$id" "$description"
    printf '[交互] 请按提示操作，具体输入以用例描述为准。\n'
    "$@"
    actual=$?
    printf '[退出码] 预期=%s 实际=%s\n' "$expected" "$actual"
    if [ "$actual" -eq "$expected" ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        printf '[通过] %s\n' "$id"
    else
        SKIP_COUNT=$((SKIP_COUNT + 1))
        printf '[观察] %s 的人工输入结果不满足预期，因密码不回显而无法确认输入内容\n' "$id"
    fi
}

# 功能：显示脚本选项与可覆盖的环境变量。
# 入参：无。
# 返回：帮助文本写到标准输出，状态来自 cat。
# 异常：无语言异常，命令或退出状态按上述规则处理。
# 注意：只打印，不解析参数；帮助内的脚本路径是已有文案，当前实际文件位于 scripts/test/full-scenario-test.sh。
usage() {
    cat <<'EOF'
用法: sh scripts/full-scenario-test.sh [选项]

选项:
  --quick                 只运行 Debug 主场景，跳过可选测试。
  --non-interactive       不提问，跳过外置卷、TTY、压力和破解测试。
  --no-build              缺少二进制时不自动构建。
  --release-full          使用 Release 构建运行完整生命周期。
  --external 路径         测试指定的已挂载外置卷目录。
  --external-cycles 次数  设置外置卷断开/重连循环次数，默认 3。
  --keep                  运行结束后保留隔离测试数据。
  --brute                 运行 veil-brute-force 交互测试。
  --stress                运行并发和较大数据测试。
  -h, --help              显示帮助。

环境变量:
  VEIL_TEST_ROOT          覆盖临时测试数据目录。
  VEIL_REPORT_ROOT        覆盖报告目录。
  VEIL_EXTERNAL_VOLUME    非交互模式下使用的外置卷挂载路径。
  VEIL_EXTERNAL_CYCLES    外置卷断开/重连循环次数。
EOF
}

# 功能：将命令行选项转换成全局测试开关并校验外置卷循环次数。
# 入参：全部位置参数；--external 和 --external-cycles 各消耗一个后续值，其余开关不带值。
# 返回：更新 QUICK、NON_INTERACTIVE 等全局变量；正常解析结束继续调用流程。
# 异常：无语言异常，命令或退出状态按上述规则处理。
# 注意：帮助直接 exit 0；未知选项、缺值和非法循环次数 exit 2。没有拒绝所有选项组合，--brute 同时开启压力测试；目录已在解析前创建，--help 也可能留下目录。
parse_args() {
    # 每轮消费一个选项；带值选项在分支内先 shift，再由轮末 shift 消费参数值。
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --quick)
                QUICK=1
                ;;
            --non-interactive)
                NON_INTERACTIVE=1
                ;;
            --no-build)
                NO_BUILD=1
                ;;
            --release-full)
                RELEASE_FULL=1
                ;;
            --external)
                shift
                if [ "$#" -eq 0 ]; then
                    printf '--external 后面缺少路径\n' >&2
                    exit 2
                fi
                EXTERNAL_DIR=$1
                ;;
            --external-cycles)
                shift
                if [ "$#" -eq 0 ]; then
                    printf '--external-cycles 后面缺少次数\n' >&2
                    exit 2
                fi
                EXTERNAL_CYCLES=$1
                ;;
            --keep)
                KEEP_DATA=1
                ;;
            --brute)
                RUN_BRUTE=1
                RUN_STRESS=1
                ;;
            --stress)
                RUN_STRESS=1
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                printf '未知选项: %s\n' "$1" >&2
                usage >&2
                exit 2
                ;;
        esac
        shift
    done

    case "$EXTERNAL_CYCLES" in
        ''|*[!0-9]*)
            printf '外置卷循环次数必须是正整数: %s\n' "$EXTERNAL_CYCLES" >&2
            exit 2
            ;;
    esac
    [ "$EXTERNAL_CYCLES" -ge 1 ] || {
        printf '外置卷循环次数必须至少为 1\n' >&2
        exit 2
    }
}

# 功能：为可选测试区域读取用户是否同意运行。
# 入参：$1 提示文本；$2 默认选项，预期 y 或 n。
# 返回：回答 y/Y/yes/YES 为 0，其余为 1；空输入或读取失败采用默认值。
# 异常：无语言异常，命令或退出状态按上述规则处理。
# 注意：NON_INTERACTIVE=1 时不读取终端，仅默认值精确为 y 才返回 0，不保证所有调用都跳过；函数不做多轮重问。
ask_yes_no() {
    prompt=$1
    default=$2
    if [ "$NON_INTERACTIVE" -eq 1 ]; then
        [ "$default" = "y" ]
        return
    fi

    if [ "$default" = "y" ]; then
        printf '%s [y/n，默认 y]: ' "$prompt"
    else
        printf '%s [y/n，默认 n]: ' "$prompt"
    fi
    IFS= read -r answer || answer=
    [ -z "$answer" ] && answer=$default
    case "$answer" in
        y|Y|yes|YES) return 0 ;;
        *) return 1 ;;
    esac
}

# 功能：通过命名管道将后续 stdout/stderr 合并后交给 tee，同时显示和追加到 run.log。
# 入参：无；读取 REPORT_ROOT、RUN_ROOT，设置 LOG_FILE、LOG_FIFO、LOG_TEE_PID，并保存原文件描述符 3/4。
# 返回：正常返回重定向状态；mkfifo 失败时仅改 LOG_FILE 名并返回，没有真的启用备用日志写入。
# 异常：Shell 无语言异常，命令失败按上述分支处理。
# 注意：不是结构化报告，也不重置计数；共享 run.log 使用追加模式，残留 FIFO 或并发运行可能导致初始化降级。
start_logging() {
    mkdir -p "$REPORT_ROOT" "$RUN_ROOT" "$RUN_ROOT/cases"
    LOG_FILE="$REPORT_ROOT/run.log"
    LOG_FIFO="$RUN_ROOT/run.fifo"

    if ! mkfifo "$LOG_FIFO" 2>/dev/null; then
        LOG_FILE="$REPORT_ROOT/run-direct.log"
        return
    fi

    tee -a "$LOG_FILE" < "$LOG_FIFO" &
    LOG_TEE_PID=$!
    exec 3>&1
    exec 4>&2
    exec > "$LOG_FIFO" 2>&1
}

# 功能：停止日志转发，恢复原 stdout/stderr 并清理命名管道。
# 入参：无；使用 start_logging 保存的进程号和文件描述符。
# 返回：存在 tee 进程号时恢复输出、关闭副本、等待 tee 并删除 FIFO；不生成最终统计或结束时间。
# 异常：Shell 无语言异常，命令失败按上述分支处理。
# 注意：忽略 tee 的失败状态；不清空 LOG_TEE_PID，调用方通过 cleanup 的幂等标记避免重复恢复已关闭描述符。
finish_logging() {
    if [ -n "${LOG_TEE_PID:-}" ]; then
        exec 1>&3 2>&4
        exec 3>&-
        exec 4>&-
        wait "$LOG_TEE_PID" 2>/dev/null || true
        rm -f "$LOG_FIFO"
    fi
}

# 功能：按保留开关收尾日志并尝试删除测试运行目录。
# 入参：无；入口立即保存上一命令的 $?，读取 KEEP_DATA、RUN_ROOT 等全局值。
# 返回：返回入口保存的状态；CLEANUP_DONE 已置位时跳过再次清理。
# 异常：Shell 无语言异常，命令失败按上述分支处理。
# 注意：rm -rf 直接作用于环境可覆盖的 RUN_ROOT，不额外验证其属于安全测试目录；删除失败仍会打印已删除。报告目录通常独立，但若调用方把它放在 RUN_ROOT 内也会一起删除。
cleanup() {
    rc=$?
    if [ "${CLEANUP_DONE:-0}" -eq 1 ]; then
        return "$rc"
    fi
    CLEANUP_DONE=1
    finish_logging
    if [ "$KEEP_DATA" -eq 0 ]; then
        rm -rf "$RUN_ROOT"
        printf '隔离测试数据已删除: %s\n' "$RUN_ROOT"
    else
        printf '隔离测试数据已保留: %s\n' "$RUN_ROOT"
    fi
    printf '测试报告: %s\n' "$LOG_FILE"
    return "$rc"
}

# 功能：创建后续测试共用的文件内容、目录树和口令字典样本。
# 入参：无；所有输出由 RUN_ROOT 推导，设置 FIXTURES。
# 返回：写入文本、空文件、二进制、30 层目录和可选链接/FIFO；最后一个字典创建命令状态作为函数状态。
# 异常：Shell 无语言异常，命令失败按上述分支处理。
# 注意：多数写入失败没有逐项检查，链接和 FIFO 创建失败被忽略；不清空已有同名目录，重复使用 RUN_ROOT 可能残留上次数据。
prepare_fixtures() {
    FIXTURES="$RUN_ROOT/fixtures"
    mkdir -p \
        "$FIXTURES" \
        "$FIXTURES/empty-dir" \
        "$FIXTURES/tree/sub/deep" \
        "$FIXTURES/中文 目录/子目录" \
        "$FIXTURES/deep"

    printf 'hello veil\n' > "$FIXTURES/hello.txt"
    : > "$FIXTURES/empty.bin"
    printf '\000\001\002\003\377\376\375\252\125\n' > "$FIXTURES/binary.bin"
    printf 'root file\n' > "$FIXTURES/tree/top.txt"
    printf 'deep file\n' > "$FIXTURES/tree/sub/deep.txt"
    printf '中文内容\n' > "$FIXTURES/中文 目录/子目录/文字 文件.txt"
    printf 'space name\n' > "$FIXTURES/name with spaces & symbols (1).txt"
    printf 'dash file\n' > "$FIXTURES/-leading-dash.txt"

    depth=1
    deep_dir="$FIXTURES/deep"
    # 每轮在上一层基础上新建一层并写入一个文件，形成嵌套路径而不是 30 个同级目录。
    while [ "$depth" -le 30 ]; do
        deep_dir="$deep_dir/level$depth"
        mkdir -p "$deep_dir"
        printf 'depth %s\n' "$depth" > "$deep_dir/file.txt"
        depth=$((depth + 1))
    done

    if command -v ln >/dev/null 2>&1; then
        ln -s "$FIXTURES/hello.txt" "$FIXTURES/tree/link-to-hello" 2>/dev/null || true
    fi
    if command -v mkfifo >/dev/null 2>&1; then
        mkfifo "$FIXTURES/fifo" 2>/dev/null || true
    fi

    dd if=/dev/zero of="$FIXTURES/one-mib.bin" bs=1048576 count=1 >/dev/null 2>&1 || \
        dd if=/dev/zero of="$FIXTURES/one-mib.bin" bs=1024 count=1024 >/dev/null 2>&1

    printf '%s\n' "1" "wrong" "letmein" "password" > "$RUN_ROOT/wordlist-hit.txt"
    printf '%s\n' "wrong" "letmein" "password" > "$RUN_ROOT/wordlist-miss.txt"
    : > "$RUN_ROOT/wordlist-empty.txt"
}

# 功能：根据二进制可执行性和修改时间决定是否构建测试工具。
# 入参：无；读取 QUICK、NO_BUILD、RUN_BRUTE 及各二进制路径。
# 返回：缺少要求的可执行文件时 exit 1，否则继续；构建状态只打印，不直接决定退出。
# 异常：Shell 无语言异常，命令失败按上述分支处理。
# 注意：构建失败但旧二进制仍可执行时可继续使用旧版；即使只发现 Release 过期也先构建 Debug CLI 和破解工具。NO_BUILD 允许过期文件，仅拒绝缺失必需文件。
ensure_binaries() {
    section "构建与前置条件检查"
    printf '项目根目录: %s\n' "$PROJECT_ROOT"
    printf 'Debug CLI: %s\n' "$DEBUG_BIN"
    printf 'Release CLI: %s\n' "$RELEASE_BIN"
    printf 'Debug 破解工具: %s\n' "$BRUTE_BIN"
    printf '操作系统: %s\n' "$(uname -a 2>/dev/null || printf 未知)"

    debug_stale=0
    release_stale=0
    brute_stale=0
    binary_is_stale "$DEBUG_BIN" && debug_stale=1
    [ "$QUICK" -eq 0 ] && binary_is_stale "$RELEASE_BIN" && release_stale=1
    [ "$RUN_BRUTE" -eq 1 ] && binary_is_stale "$BRUTE_BIN" && brute_stale=1

    if [ "$NO_BUILD" -eq 0 ]; then
        if [ "$debug_stale" -eq 1 ] || [ "$brute_stale" -eq 1 ] || [ "$release_stale" -eq 1 ]; then
            printf '\n正在构建 Debug CLI 和破解工具...\n'
            (cd "$PROJECT_ROOT" && cargo build -p veil-cli -p veil-brute-force)
            build_debug_rc=$?
            printf '[构建-Debug] 退出码=%s\n' "$build_debug_rc"

            if [ "$release_stale" -eq 1 ]; then
                printf '\n正在构建 Release CLI...\n'
                (cd "$PROJECT_ROOT" && cargo build --release -p veil-cli)
                build_release_rc=$?
                printf '[构建-Release] 退出码=%s\n' "$build_release_rc"
            fi
        else
            printf '已找到现成二进制，跳过构建。\n'
        fi
    else
        printf '已通过 --no-build 禁用构建。\n'
    fi

    missing=0
    [ -x "$DEBUG_BIN" ] || { printf '缺少可执行文件: %s\n' "$DEBUG_BIN"; missing=1; }
    if [ "$QUICK" -eq 0 ]; then
        [ -x "$RELEASE_BIN" ] || { printf '缺少可执行文件: %s\n' "$RELEASE_BIN"; missing=1; }
    fi
    if [ "$RUN_BRUTE" -eq 1 ] && [ ! -x "$BRUTE_BIN" ]; then
        printf '缺少可执行文件: %s\n' "$BRUTE_BIN"
        missing=1
    fi
    if [ "$missing" -eq 1 ]; then
        printf '必需的二进制文件不可用。\n' >&2
        exit 1
    fi

    printf '\n二进制文件信息:\n'
    file "$DEBUG_BIN" "$RELEASE_BIN" 2>/dev/null || true
}

# 功能：在构建前确定外置卷、TTY、压力与口令演示等可选测试开关。
# 入参：无；读取全局选项和外置卷路径，交互模式从标准输入读取回答。
# 返回：更新 RUN_EXTERNAL、RUN_TTY、RUN_STRESS、RUN_BRUTE；不产生业务数据返回。
# 异常：输入结束按空回答或默认项处理；注意：非交互时有非空 EXTERNAL_DIR 就启用外置卷，不在此验证目录；已由参数开启的其他测试标记也不会统一清零。
prompt_optional_areas() {
    if [ "$NON_INTERACTIVE" -eq 1 ]; then
        if [ -n "$EXTERNAL_DIR" ]; then
            RUN_EXTERNAL=1
        fi
        return
    fi

    section "交互测试配置"
    printf '自动测试密码为 1；交互密码测试会要求你在终端输入 1。\n'

    if [ -z "$EXTERNAL_DIR" ]; then
        printf '可选外置/可移动卷路径（留空跳过）: '
        IFS= read -r EXTERNAL_DIR || EXTERNAL_DIR=
    fi
    if [ -n "$EXTERNAL_DIR" ]; then
        if [ -d "$EXTERNAL_DIR" ]; then
            RUN_EXTERNAL=1
        else
            printf '路径不是目录，跳过外置卷测试: %s\n' "$EXTERNAL_DIR"
        fi
    fi

    if ask_yes_no "运行交互式 TTY/密码输入检查" n; then
        RUN_TTY=1
    fi
    if [ "$QUICK" -eq 0 ] && ask_yes_no "运行并发和较大数据检查" y; then
        RUN_STRESS=1
    fi
    if [ "$QUICK" -eq 0 ] && ask_yes_no "运行 veil-brute-force 场景" n; then
        RUN_BRUTE=1
        if [ ! -x "$BRUTE_BIN" ] && [ "$NO_BUILD" -eq 1 ]; then
            printf '破解工具不存在，将跳过相关场景。\n'
            RUN_BRUTE=0
        fi
    fi

    printf '\n测试脚本将使用:\n'
    printf '  测试数据: %s\n' "$RUN_ROOT"
    printf '  测试报告: %s\n' "$REPORT_ROOT/run.log"
    printf '  外置卷:   %s\n' "${EXTERNAL_DIR:-<未启用>}"
    printf '\n按回车开始，按 Ctrl-C 取消: '
    IFS= read -r _ || true
}

# 功能：检查 CLI 帮助、语言、参数冲突和异常输出通道的预期退出状态。
# 入参：$1 阶段标签（如 debug）；$2 可选二进制路径，缺省 DEBUG_BIN；返回通过 run_case 累计结果，不返回场景是否全部成功。
# 异常：被测命令错误由 run_case 记录并继续；注意：多数用例只验退出码，不断言帮助正文或 stdout/stderr 分流，断管道用例还依赖 Perl。
run_global_help_section() {
    section "GLB：入口、帮助、本地化、参数解析和输出通道"
    phase "global-$1"
    BIN=${2:-$DEBUG_BIN}

    run_case "GLB-01" 0 "新 HOME 首次启动和帮助" env -u VEIL_PASSWORD VEIL_HINTS=off "$BIN" --help
    run_case "GLB-05" 2 "无子命令时返回参数错误" env VEIL_HINTS=off "$BIN"
    run_case "GLB-06" 0 "顶层中文帮助" env VEIL_LANG=zh VEIL_HINTS=off "$BIN" --help
    run_case "GLB-06-en" 0 "顶层英文帮助" env VEIL_LANG=en VEIL_HINTS=off "$BIN" --help
    run_case "GLB-07" 0 "输出全部子命令帮助" env VEIL_HINTS=off "$BIN" help all
    run_case "GLB-08" 0 "文件类型帮助" env VEIL_HINTS=off "$BIN" help files
    run_case "GLB-08-en" 0 "英文文件类型帮助" env VEIL_LANG=en VEIL_HINTS=off "$BIN" help files
    run_case "GLB-09" 0 "--all 输出全部帮助" env VEIL_HINTS=off "$BIN" help --all
    run_case "GLB-10" 1 "未知帮助主题" env VEIL_HINTS=off "$BIN" help unknown-topic
    run_case "GLB-11" 0 "版本信息" "$BIN" --version

    # 每轮一个固定帮助主题，每个主题独立生成 stdout/stderr 报告，确认相同帮助入口能接受这些名称。
    for topic in init add rm mv free ex info list exists passwd shell pack unpack config link help; do
        run_case "GLB-07-$topic" 0 "$topic 子命令帮助" "$BIN" help "$topic"
    done

    run_case "GLB-12-narrow" 0 "窄终端帮助输出" env COLUMNS=20 VEIL_HINTS=off "$BIN" --help
    run_case "GLB-12-wide" 0 "宽终端帮助输出" env COLUMNS=240 VEIL_HINTS=off "$BIN" --help
    run_case "GLB-13-no-color" 0 "关闭颜色后的帮助输出" env NO_COLOR=1 VEIL_HINTS=off "$BIN" --help
    run_case "GLB-14-ok" 0 "标准输出和标准错误分流" env VEIL_HINTS=off "$BIN" --version
    run_case "GLB-21" 2 "未知选项" env VEIL_HINTS=off "$BIN" init demo --bogus-option
    run_case "GLB-22" 2 "未知子命令" env VEIL_HINTS=off "$BIN" definitely-not-a-command
    run_case "GLB-27" 2 "缺少容器参数" env VEIL_HINTS=off "$BIN" add
    run_case "GLB-27-source" 2 "缺少添加源路径" env VEIL_HINTS=off "$BIN" add veil-only
    run_case "GLB-27-output" 2 "缺少导出输出路径" env VEIL_HINTS=off "$BIN" ex veil-only input.txt
    run_case "GLB-29-invalid-lang" 0 "不支持的语言正常回退" env VEIL_LANG=zz-ZZ VEIL_HINTS=off "$BIN" --help

    run_case "GLB-19-conflict" 2 "位置密码和选项密码冲突" \
        env VEIL_HINTS=off "$BIN" init conflict one -p two
    run_case "GLB-20-add-conflict" 2 "add 输入路径的位置和选项写法冲突" \
        env VEIL_HINTS=off "$BIN" add c pos -i other
    run_case "GLB-25-repeat" 2 "重复普通选项的行为可观察" \
        env VEIL_HINTS=off "$BIN" init repeated -p one -p two

    run_case "GLB-23-space" 0 "包含空格的参数值正确传入" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$BIN" init "name with spaces"
    run_case "GLB-24-dash" 0 "短横线开头的合法位置参数" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$BIN" init -- -dash-name

    run_case "GLB-15-broken-pipe" 1 \
        "标准输出管道提前关闭时返回稳定 I/O 错误" \
        env VEIL_HINTS=off perl -e \
        'pipe(my $reader, my $writer) or die "pipe: $!"; close($reader); my $pid = fork(); die "fork: $!" unless defined $pid; if ($pid == 0) { open(STDOUT, ">&", $writer) or die "stdout: $!"; exec @ARGV; exit 127; } close($writer); waitpid($pid, 0); exit($? >> 8);' \
        "$BIN" --help

    run_case "GLB-26-recovery" 0 "参数错误后的正常请求不受污染" \
        env VEIL_LANG=en VEIL_HINTS=off "$BIN" help files
}

# 功能：构造并检查口令、容器名称、链接位置和工作区布局的初始化边界。
# 入参：无；使用 DEBUG_BIN 和 RUN_ROOT，phase(init) 切换 HOME 与工作目录；返回最后权限展示命令状态，具体成败由场景与断言计数保存。
# 异常：命名工作区配置失败显式 return 1，其余多数失败登记后继续；注意：服务命令会真实写盘，VEIL_TEST_KDF=fast 只用于测试，不表示生产口令派生强度。
run_init_section() {
    section "PWD/INIT：密码、容器初始化和工作区布局"
    phase "init"

    run_case "PWD-03-empty" 1 "拒绝空的新密码" \
        env VEIL_PASSWORD= VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init empty-password
    assert_file_missing "PWD-03-link" "空密码失败后不应生成链接" "$PHASE_WORK/empty-password.veil-link"

    run_case "PWD-05-unicode" 0 "使用 Unicode 密码初始化" \
        env VEIL_PASSWORD='密🔐é' VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init unicode-pass
    run_case "PWD-05-unicode-open" 0 "使用 Unicode 密码重新打开" \
        env VEIL_PASSWORD='密🔐é' VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list unicode-pass

    run_case "PWD-07-leading-space" 0 "初始化时保留密码首尾空格" \
        env VEIL_PASSWORD=' secret ' VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init spaced-pass
    run_case "PWD-07-leading-space-open" 0 "打开时保留密码首尾空格" \
        env VEIL_PASSWORD=' secret ' VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list spaced-pass
    run_case "PWD-07-trimmed-fails" 1 "去除空格后的密码无法打开" \
        env VEIL_PASSWORD='secret' VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list spaced-pass

    # 构造 1024 个 ASCII x，验证较长口令能初始化和重开；只覆盖这一长度，不证明无任何长度上限。
    LONG_PASSWORD=$(awk 'BEGIN { for (i = 0; i < 1024; i++) printf "x" }')
    run_case "PWD-06-long" 0 "使用 1024 字节密码初始化" \
        env VEIL_PASSWORD="$LONG_PASSWORD" VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init long-pass
    run_case "PWD-06-long-open" 0 "使用 1024 字节密码重新打开" \
        env VEIL_PASSWORD="$LONG_PASSWORD" VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list long-pass

    run_case "INIT-01-default" 0 "默认容器初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=full VEIL_TEST_KDF=fast "$DEBUG_BIN" init core
    assert_file_exists "INIT-01-link" "core 链接存在" "$PHASE_WORK/core.veil-link"
    assert_file_exists "INIT-01-meta" "默认工作区包含元数据" \
        "$PHASE_HOME/.veil/works/default"
    assert_file_exists "INIT-01-config" "全局配置已创建" "$PHASE_HOME/.veil/config.jsonc"

    run_case "INIT-02-empty-list" 0 "新建容器列表为空" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list core

    run_case "INIT-03-unicode-name" 0 "中文、空格和符号容器名" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init "中文 容器.v1-test"
    run_case "INIT-04-blank-name" 1 "拒绝纯空白容器名" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init "   "

    run_case "INIT-05-duplicate-first" 0 "首次创建同名容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init duplicate
    run_case "INIT-05-duplicate-second" 0 "同名容器生成独立链接" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init duplicate
    printf '[观察] 同名容器生成的链接:\n'
    find "$PHASE_WORK" -maxdepth 1 -name 'duplicate*.veil-link' -print | LC_ALL=C sort

    run_case "INIT-08-parent-created" 0 "自动创建自定义链接的父目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init nested-link \
        --link "$PHASE_WORK/new/links/nested-link.veil-link"
    assert_file_exists "INIT-08-file" "深层父目录中的链接存在" "$PHASE_WORK/new/links/nested-link.veil-link"

    run_case "INIT-09-existing-link" 1 "拒绝覆盖已有链接" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init existing-link \
        --link "$PHASE_WORK/core.veil-link"
    run_case "INIT-10-bad-extension" 1 "拒绝非 .veil-link 扩展名" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init bad-ext \
        --link "$PHASE_WORK/bad-ext.txt"
    run_case "INIT-11-link-target" 0 "从链接文件名推导容器名称" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init \
        "$PHASE_WORK/derived.veil-link"

    run_case "INIT-13-default-root" 0 "显式指定默认命名工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init named-default \
        --work default
    run_case "INIT-14-missing-named" 1 "拒绝不存在的命名工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init missing-work \
        --work does-not-exist

    # 将 lab 名称登记到隔离配置后，再以名称创建容器；目录路径与可读工作区名是两个不同输入。
    CUSTOM_ROOT="$RUN_ROOT/custom-work"
    mkdir -p "$CUSTOM_ROOT"
    jsonc_tool work "$PHASE_HOME/.veil/config.jsonc" lab "$CUSTOM_ROOT" || return 1
    run_case "INIT-14-named" 0 "使用已注册的命名工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init named-custom \
        --work lab

    # 专属目录样本分别覆盖空目录、非空目录、缺失目录和普通文件，检查业务接受/拒绝条件。
    DEDICATED="$RUN_ROOT/dedicated-empty"
    mkdir -p "$DEDICATED"
    run_case "INIT-19-dedicated-empty" 0 "使用空目录作为专属工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated \
        --work-root "$DEDICATED" --dedicated --link "$PHASE_WORK/dedicated.veil-link"
    assert_file_exists "INIT-19-meta" "专属工作区按 veil_id 保存容器元数据" "$(resolve_work_for_link "$PHASE_WORK/dedicated.veil-link")/.veil-info"

    NONEMPTY="$RUN_ROOT/dedicated-nonempty"
    mkdir -p "$NONEMPTY"
    printf 'keep\n' > "$NONEMPTY/keep.txt"
    printf 'keep\n' > "$RUN_ROOT/dedicated-keep-expected.txt"
    run_case "INIT-20-dedicated-nonempty" 1 "拒绝非空专属工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-nonempty \
        --work-root "$NONEMPTY" --dedicated --link "$PHASE_WORK/dedicated-nonempty.veil-link"
    assert_file_equals "INIT-20-preserved" "原有专属目录文件保持不变" \
        "$NONEMPTY/keep.txt" "$RUN_ROOT/dedicated-keep-expected.txt"

    DEDICATED_NEW="$RUN_ROOT/dedicated-new"
    run_case "INIT-21-dedicated-create" 0 "创建不存在的专属工作区目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-new \
        --work-root "$DEDICATED_NEW" --dedicated --link "$PHASE_WORK/dedicated-new.veil-link"

    DEDICATED_FILE="$RUN_ROOT/dedicated-file"
    printf 'not a directory\n' > "$DEDICATED_FILE"
    run_case "INIT-22-dedicated-file" 1 "拒绝将普通文件作为专属工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-file \
        --work-root "$DEDICATED_FILE" --dedicated --link "$PHASE_WORK/dedicated-file.veil-link"

    run_case "INIT-23-conflict" 1 "命名工作区和显式路径冲突" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init conflict-work \
        --work default --work-root "$RUN_ROOT/conflict"
    run_case "INIT-24-dedicated-no-path" 1 "专属模式缺少路径时拒绝" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-no-path \
        --dedicated
    run_case "INIT-25-portable-conflict" 1 "便携模式与工作区选项冲突" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init portable-conflict \
        --portable --work-root "$RUN_ROOT/portable-conflict"

    PORTABLE="$RUN_ROOT/portable"
    mkdir -p "$PORTABLE"
    run_case "INIT-26-portable" 0 "显式便携布局" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init portable \
        --portable --link "$PORTABLE/portable.veil-link"
    assert_file_exists "INIT-26-work" "本机便携选项使用固定默认工作区" \
        "$PHASE_HOME/.veil/works/default"

    # 在子 Shell 中用相对工作区路径初始化，再换回另一目录打开链接，检查位置解析是否依赖原 cwd。
    # 此手工场景最终只按跟随打开的退出码计数，首次 init 退出码未单独计入。
    REL_ROOT="$RUN_ROOT/relative-work-parent"
    mkdir -p "$REL_ROOT"
    (
        cd "$REL_ROOT" || exit 1
        VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init relative \
            --work-root relative-work --link "$REL_ROOT/relative.veil-link"
    ) > "$RUN_ROOT/cases/INIT-18.stdout" 2> "$RUN_ROOT/cases/INIT-18.stderr"
    printf '\n[用例 INIT-18] 相对显式工作区固定为稳定绝对路径\n'
    cat "$RUN_ROOT/cases/INIT-18.stdout"
    cat "$RUN_ROOT/cases/INIT-18.stderr" >&2
    (cd "$PHASE_WORK" && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$REL_ROOT/relative.veil-link") \
        > "$RUN_ROOT/cases/INIT-18-follow.stdout" 2> "$RUN_ROOT/cases/INIT-18-follow.stderr"
    follow_rc=$?
    printf '[退出码] 预期=0 实际=%s\n' "$follow_rc"
    cat "$RUN_ROOT/cases/INIT-18-follow.stdout"
    cat "$RUN_ROOT/cases/INIT-18-follow.stderr" >&2
    if [ "$follow_rc" -eq 0 ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
        printf '[通过] INIT-18\n'
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        printf '[失败] INIT-18\n'
    fi

    # 用普通文件挡住链接父目录，触发初始化失败；这里只检查退出码，没有直接断言工作区内容已回滚。
    BLOCKER="$RUN_ROOT/blocker"
    printf 'blocker\n' > "$BLOCKER"
    run_case "INIT-29-rollback" 1 "链接注册失败时回滚工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init rollback \
        --link "$BLOCKER/nested.veil-link"

    # 权限只打印供观察，下面 stat 失败也忽略，不据此增加断言失败计数。
    printf '\n[权限] 初始化产物\n'
    stat -f '%Sp %N' "$PHASE_WORK/core.veil-link" "$PHASE_HOME/.veil/config.jsonc" 2>/dev/null || \
        stat -c '%A %n' "$PHASE_WORK/core.veil-link" "$PHASE_HOME/.veil/config.jsonc" 2>/dev/null || true
}

# 功能：创建 fileops 容器，检查多种来源和目标路径的导入、浏览查询及明文导出。
# 入参：无；依赖 prepare_fixtures 生成的样本和 DEBUG_BIN，内部切到 fileops 隔离阶段。
# 返回：通过场景/断言计数保存结果，函数状态来自最后一次字节比较，不代表整组结果。
# 异常：命令失败多数记录后继续；注意：导出内容有独立 cmp 断言，但列表、目录跳过和部分错误分类只校验退出码，不保证描述的所有细节均被验证。
run_add_view_section() {
    section "ADD/VIEW/EX：文件生命周期、列表、树形、信息、存在性和导出"
    phase "fileops"

    run_case "ADD-01" 0 "使用源文件名添加文本" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init fileops
    run_case "ADD-01-add" 0 "添加普通文本" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/hello.txt"
    run_case "ADD-02" 0 "添加并重命名文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/hello.txt" renamed.txt
    run_case "ADD-03" 0 "添加多级目标路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/hello.txt" nested/deep/file.txt
    run_case "ADD-04" 0 "目录目标以斜杠结尾时保留源文件名" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/hello.txt" slash/
    run_case "ADD-05" 0 "添加空文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/empty.bin"
    run_case "ADD-06" 0 "添加二进制文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/binary.bin"
    run_case "ADD-08" 0 "添加 Unicode 路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops \
        "$FIXTURES/中文 目录/子目录/文字 文件.txt" "中文 目录/子目录/文字 文件.txt"
    run_case "ADD-09" 0 "添加深层目录树" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/deep"
    run_case "ADD-10" 0 "添加目录并保留源目录前缀" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/tree"
    run_case "ADD-11" 0 "添加目录并替换目标前缀" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/tree" imported/tree
    run_case "ADD-12" 0 "空目录不生成文件条目" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/empty-dir"
    # 符号链接和 FIFO 是否实际存在取决于夹具创建；这些命令没有超时，“不阻塞”不是受时间限制的自动断言。
    run_case "ADD-13" 0 "目录内符号链接不会被跟随" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/tree" symlink-tree
    run_case "ADD-14" 0 "包含 FIFO 的目录可跳过且不阻塞" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES"
    run_case "ADD-15" 1 "拒绝将单个 FIFO 作为文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/fifo"
    run_case "ADD-16" 1 "拒绝不存在的源路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$RUN_ROOT/does-not-exist"
    run_case "ADD-20" 0 "规范化 ./ 目标路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops \
        "$FIXTURES/name with spaces & symbols (1).txt" "./dir/name with spaces & symbols (1).txt"

    run_case "ADD-21-parent" 1 "拒绝父目录穿越目标" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops \
        "$FIXTURES/hello.txt" "../escape.txt"
    run_case "ADD-21-absolute" 1 "拒绝绝对目标路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops \
        "$FIXTURES/hello.txt" "/absolute.txt"
    run_case "ADD-21-empty" 1 "拒绝空目标路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops \
        "$FIXTURES/hello.txt" ""

    # 同一来源路径先写旧内容再改成新内容，测试同名目标更新；记录旧密文数量，但此函数未用它断言旧密文清理。
    printf 'replacement-first\n' > "$RUN_ROOT/replace.txt"
    run_case "ADD-22-first" 0 "替换前先添加文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$RUN_ROOT/replace.txt"
    before_enc_count=$(find "$(resolve_work_for_link "$PHASE_WORK/fileops.veil-link")" -name '*.enc' | wc -l | tr -d ' ')
    printf 'replacement-second\n' > "$RUN_ROOT/replace.txt"
    run_case "ADD-22-replace" 0 "替换已有目标" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$RUN_ROOT/replace.txt"

    run_case "ADD-26-wrong-password" 1 "错误密码添加失败" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/hello.txt" wrong-add.txt

    # 查询场景复用刚导入的数据，逐条检查退出状态；stdout/stderr 写入报告，未在本组解析列表完整性。
    run_case "VIEW-02-list" 0 "列出含文件的容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list fileops
    run_case "VIEW-08-free" 0 "树形显示含文件的容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" free fileops
    run_case "VIEW-15-info" 0 "显示容器信息" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" info fileops
    run_case "VIEW-22-exists-file" 0 "检查存在的文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" exists fileops nested/deep/file.txt
    run_case "VIEW-23-exists-dir" 0 "检查存在的目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" exists fileops nested/deep
    run_case "VIEW-24-exists-missing" 1 "检查不存在的路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" exists fileops missing.txt
    run_case "VIEW-25-exists-root" 0 "检查容器根目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" exists fileops "."
    run_case "VIEW-26-exists-dot" 0 "规范化 ./ 前缀" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" exists fileops ./nested/deep/file.txt
    run_case "VIEW-27-exists-parent" 1 "exists 拒绝父目录穿越" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" exists fileops ../escape
    run_case "VIEW-29-exists-wrong-password" 1 "错误密码不能误报为不存在" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" exists fileops nested/deep/file.txt

    # 输出位置均在本次运行目录；每组成功导出随后按字节与对应源样本比较，避免只凭退出码判断内容正确。
    OUT="$RUN_ROOT/export/hello.txt"
    mkdir -p "$RUN_ROOT/export"
    run_case "EX-01" 0 "导出文本文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops hello.txt "$OUT"
    assert_file_equals "EX-01-bytes" "导出的文本与源文件一致" "$OUT" "$FIXTURES/hello.txt"
    run_case "EX-02-empty" 0 "导出空文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops empty.bin "$RUN_ROOT/export/empty.bin"
    assert_file_equals "EX-02-empty-bytes" "空文件导出内容一致" "$RUN_ROOT/export/empty.bin" "$FIXTURES/empty.bin"
    run_case "EX-02-binary" 0 "导出二进制文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops binary.bin "$RUN_ROOT/export/binary.bin"
    assert_file_equals "EX-02-binary-bytes" "二进制文件导出内容一致" "$RUN_ROOT/export/binary.bin" "$FIXTURES/binary.bin"
    run_case "EX-03-unicode" 0 "导出 Unicode 深层路径文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops \
        "中文 目录/子目录/文字 文件.txt" "$RUN_ROOT/export/中文 文件.txt"
    assert_file_equals "EX-03-unicode-bytes" "Unicode 文件导出内容一致" \
        "$RUN_ROOT/export/中文 文件.txt" "$FIXTURES/中文 目录/子目录/文字 文件.txt"
    printf 'old output\n' > "$RUN_ROOT/export/overwrite.txt"
    run_case "EX-05-overwrite" 0 "覆盖已有输出文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops hello.txt "$RUN_ROOT/export/overwrite.txt"
    assert_file_equals "EX-05-overwrite-bytes" "覆盖后的输出与源文件一致" \
        "$RUN_ROOT/export/overwrite.txt" "$FIXTURES/hello.txt"
    run_case "EX-04-missing-parent" 0 "输出父目录不存在时自动创建" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops hello.txt "$RUN_ROOT/no-such-parent/x.bin"
    assert_file_equals "EX-04-missing-parent-bytes" "自动创建父目录后导出内容一致" \
        "$RUN_ROOT/no-such-parent/x.bin" "$FIXTURES/hello.txt"
    run_case "EX-07-missing" 1 "导出不存在的容器内文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops missing.bin "$RUN_ROOT/export/missing.bin"
    assert_file_missing "EX-07-no-file" "导出不存在文件时不应创建输出" "$RUN_ROOT/export/missing.bin"
    # 预先放置哨兵文本，错误密码导出后再次比较，确认拒绝请求没有覆盖已有目标。
    printf 'sentinel\n' > "$RUN_ROOT/export/wrong-password.txt"
    run_case "EX-06-wrong-password" 1 "错误密码在覆盖输出前失败" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops hello.txt "$RUN_ROOT/export/wrong-password.txt"
    printf 'sentinel\n' > "$RUN_ROOT/export/wrong-password-expected.txt"
    assert_file_equals "EX-06-sentinel" "错误密码导出不修改目标文件" \
        "$RUN_ROOT/export/wrong-password.txt" "$RUN_ROOT/export/wrong-password-expected.txt"

    run_case "EX-02-mib" 0 "添加并导出 1 MiB 文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/one-mib.bin"
    run_case "EX-02-mib-export" 0 "导出 1 MiB 文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops one-mib.bin "$RUN_ROOT/export/one-mib.bin"
    assert_file_equals "EX-02-mib-bytes" "1 MiB 文件导出内容一致" \
        "$RUN_ROOT/export/one-mib.bin" "$FIXTURES/one-mib.bin"
}

# 功能：用项目配置规则把链接解析为实际密文工作目录。
# 入参：$1 链接路径；返回：不是普通文件时返回 1，否则透传 jsonc_tool resolve 的路径输出与退出状态。
# 异常：辅助程序读取、解析和配置错误由非零状态报告；注意：使用当前阶段 HOME 的配置，不能脱离阶段环境独立解释链接。
resolve_work_for_link() {
    link=$1
    if [ ! -f "$link" ]; then
        return 1
    fi
    jsonc_tool resolve "$link" ''
}

# 功能：检查旧格式 config.jsonc、.veil-link、.veil-info 及 .enc / .veil 产物的结构、大小和改密前后变化。
# 入参：无；切换 persistence 阶段，并使用真实 Debug CLI 创建样本；返回最后一组产物展示管道状态，断言结果写入全局计数。
# 异常：多数命令失败后继续；注意：该组按硬编码的旧文件布局与版本解析，不是直接检查 app.db，也不模拟完整进程崩溃恢复。
# 标记未出现在密文中只证明特定明文片段未被直接找到，不能独立证明整体加密安全性。
run_persistence_artifact_section() {
    section "CONF/ART：三个核心配置文件和运行产物内容检查"
    phase "persistence"

    # 固定公开标记用于在链接、元数据、密文和包里查找样本内容泄露；不是用户真实秘密。
    ARTIFACT_SECRET="VEIL_PLAINTEXT_MARKER_20260912"
    FIRST_SOURCE="$RUN_ROOT/artifact-first.txt"
    SECOND_SOURCE="$RUN_ROOT/artifact-second.txt"
    REPLACEMENT_SOURCE="$RUN_ROOT/artifact-first-replaced.txt"
    printf '%s\n' "$ARTIFACT_SECRET first version" > "$FIRST_SOURCE"
    printf '%s\n' "$ARTIFACT_SECRET second file" > "$SECOND_SOURCE"
    printf '%s\n' "$ARTIFACT_SECRET replacement version" > "$REPLACEMENT_SOURCE"

    run_case "CONF-00" 0 "初始化三个核心配置文件测试容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init artifact-check

    # 三个路径分别定位链接正文、全局注册配置和容器元数据；工作区通过链接及当前阶段配置解析。
    ARTIFACT_LINK="$PHASE_WORK/artifact-check.veil-link"
    ARTIFACT_CONFIG="$PHASE_HOME/.veil/config.jsonc"
    ARTIFACT_WS=$(resolve_work_for_link "$ARTIFACT_LINK")
    ARTIFACT_META="$ARTIFACT_WS/.veil-info"
    LINK_HASH_BEFORE=$(hash_file "$ARTIFACT_LINK")

    assert_file_exists "CONF-01" "全局配置存在" "$ARTIFACT_CONFIG"
    assert_file_exists "CONF-01-link" "链接文件存在" "$ARTIFACT_LINK"
    assert_file_exists "CONF-01-meta" "元数据文件存在" "$ARTIFACT_META"
    assert_contains "CONF-01-version" "配置包含版本" '"version":' "$ARTIFACT_CONFIG"
    assert_contains "CONF-02-veil" "配置包含容器记录" '"veils":' "$ARTIFACT_CONFIG"
    assert_contains "CONF-02-id" "容器记录包含稳定 ID" '"veil_id":' "$ARTIFACT_CONFIG"
    assert_not_contains "CONF-03-volume" "配置不保存卷记录" '"volumes":' "$ARTIFACT_CONFIG"
    assert_contains "CONF-04-link-record" "配置包含独立链接正文" '"link":' "$ARTIFACT_CONFIG"

    LINK_ID=$(jsonc_tool get "$ARTIFACT_LINK" /veil/veil_id)
    LINK_NAME=$(jsonc_tool get "$ARTIFACT_LINK" /veil/veil_name)
    META_STRINGS="$RUN_ROOT/cases/CONF-09-meta-strings.txt"
    strings "$ARTIFACT_META" > "$META_STRINGS"
    META_ID=$(grep -Eo 'veil-[0-9a-f]+' "$META_STRINGS" | head -n 1)
    assert_values_equal "CONF-09-id" "链接与元数据稳定 ID 一致" "$LINK_ID" "$META_ID"
    assert_contains "CONF-09-name" "元数据明文头包含容器名称" "$LINK_NAME" "$META_STRINGS"
    assert_contains "CONF-09-created-at" "元数据明文头保留容器创建时间" \
        "$(jsonc_tool get "$ARTIFACT_LINK" /veil/created_at)" "$META_STRINGS"
    assert_values_equal "CONF-07-version" "链接包含版本号" "1.0" "$(jsonc_tool get "$ARTIFACT_LINK" /version)"
    assert_not_contains "CONF-07-no-work" "链接不保存工作区" '"work":' "$ARTIFACT_LINK"
    assert_not_contains "CONF-07-no-work-id" "配置不保存工作区 ID" '"work_id":' "$ARTIFACT_CONFIG"
    assert_values_equal "CONF-04-link-body" "链接缓存复用完整链接正文" \
        "$(jsonc_tool get "$ARTIFACT_LINK" '')" "$(jsonc_tool get "$ARTIFACT_CONFIG" /links/0/link)"
    assert_not_contains "CONF-07-no-encryption" "链接不包含加密方案字段" '"encryption":' "$ARTIFACT_LINK"
    assert_not_contains "CONF-07-no-metadata" "链接不包含展示元数据字段" '"metadata":' "$ARTIFACT_LINK"
    assert_not_contains "CONF-08-secret" "链接不包含明文密码或内容标记" "$ARTIFACT_SECRET" "$ARTIFACT_LINK"

    assert_values_equal "CONF-08-relative" "默认工作区相对于配置目录" \
        "works/default" "$(jsonc_tool get "$ARTIFACT_CONFIG" /work/default/path)"

    printf 'VEILMETA' > "$RUN_ROOT/expected-meta-magic.bin"
    dd if="$ARTIFACT_META" of="$RUN_ROOT/actual-meta-magic.bin" bs=1 count=8 2>/dev/null
    assert_file_equals "CONF-09-magic" "元数据魔数正确" \
        "$RUN_ROOT/actual-meta-magic.bin" "$RUN_ROOT/expected-meta-magic.bin"
    META_TOTAL=$(file_size "$ARTIFACT_META")
    META_HEADER_LEN=$(dd if="$ARTIFACT_META" bs=1 skip=8 count=2 2>/dev/null | od -An -tu2 | tr -d ' \n')
    META_MIN_SIZE=$((10 + META_HEADER_LEN))
    if [ "$META_MIN_SIZE" -le "$META_TOTAL" ]; then
        printf '[断言通过] %-10s 元数据头长度未越界（%s <= %s）\n' "CONF-09-length" "$META_MIN_SIZE" "$META_TOTAL"
        CHECK_COUNT=$((CHECK_COUNT + 1))
    else
        printf '[断言失败] %-10s 元数据头长度越界（%s > %s）\n' "CONF-09-length" "$META_MIN_SIZE" "$META_TOTAL"
        CHECK_COUNT=$((CHECK_COUNT + 1))
        ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    fi
    # 将元数据头的字节列表交给 awk：先积累 byte[1..count]，再逐个读取 tag、两字节小端长度及对应值。
    # tag 是字段类型，len 是字段内容长度；pos 每次跳过 3 字节字段头和 len 字节内容，未进行完整损坏输入边界校验。
    META_TLV_FIELDS=$(dd if="$ARTIFACT_META" bs=1 count="$META_MIN_SIZE" 2>/dev/null | od -An -tu1 -v | awk '
        {
            for (i = 1; i <= NF; i++) {
                byte[++count] = $i
            }
        }
        END {
            pos = 11
            while (pos <= count) {
                tag = byte[pos]
                len = byte[pos + 1] + byte[pos + 2] * 256
                if (tag == 1 && len == 2) version = byte[pos + 3] + byte[pos + 4] * 256
                if (tag == 2) salt_len = len
                if (tag == 3 && len == 1) algorithm = byte[pos + 3]
                if (tag == 5) nonce_len = len
                pos += 3 + len
            }
            printf "version=%s algorithm=%s salt_len=%s nonce_len=%s", version, algorithm, salt_len, nonce_len
        }
    ')
    printf '%s\n' "$META_TLV_FIELDS" > "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_contains "CONF-09-version" "元数据 TLV 版本号可解析" "version=6" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_contains "CONF-09-algorithm" "元数据 TLV 算法标识正确" "algorithm=2" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_contains "CONF-09-salt" "元数据 TLV 盐长度为 32" "salt_len=32" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_contains "CONF-09-nonce" "元数据 TLV nonce 长度为 12" "nonce_len=12" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_not_contains "CONF-10-meta-secret" "元数据不包含明文内容标记" "$ARTIFACT_SECRET" "$ARTIFACT_META"

    assert_values_equal "CONF-05-link-body" "配置链接正文与链接文件全部字段值一致" \
        "$(jsonc_tool get "$ARTIFACT_CONFIG" /links/0/link)" "$(jsonc_tool get "$ARTIFACT_LINK" '')"
    assert_not_contains "CONF-06-no-raw" "配置不保存链接原始字节" '"raw_hex":' "$ARTIFACT_CONFIG"
    assert_not_contains "CONF-06-no-hash" "配置不保存链接内容哈希" '"content_hash":' "$ARTIFACT_CONFIG"

    assert_private_mode "CONF-15-config-mode" "全局配置仅当前用户可读写" "$ARTIFACT_CONFIG"
    assert_private_mode "CONF-15-link-mode" "链接文件仅当前用户可读写" "$ARTIFACT_LINK"
    assert_private_mode "CONF-15-meta-mode" "元数据仅当前用户可读写" "$ARTIFACT_META"

    run_case "ART-01-add" 0 "添加第一个可识别明文文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$FIRST_SOURCE" first.txt
    ENC_COUNT=$(count_enc_files "$ARTIFACT_WS")
    assert_values_equal "ART-01-count" "单文件添加后密文数量增加一" "1" "$ENC_COUNT"
    FIRST_SIZE=$(file_size "$FIRST_SOURCE")
    # 单文件阶段取首个密文检查长度；顺序由 find 决定，这里依赖前一步数量断言确实只有一个文件。
    FIRST_ENC=$(find "$ARTIFACT_WS" -maxdepth 1 -type f -name '*.enc' | head -n 1)
    ENC_SIZE=$(file_size "$FIRST_ENC")
    assert_values_equal "ART-01-tag" "密文大小等于明文加 16 字节认证标签" "$((FIRST_SIZE + 16))" "$ENC_SIZE"
    assert_not_contains "ART-03-enc-secret" "密文不含明文内容标记" "$ARTIFACT_SECRET" "$FIRST_ENC"

    # 保存错误密码操作前的三份字节摘要，随后比较不变性；这能发现变动，但不覆盖并发写入或所有失败类型。
    META_HASH_BEFORE_FAIL=$(hash_file "$ARTIFACT_META")
    CONFIG_HASH_BEFORE_FAIL=$(hash_file "$ARTIFACT_CONFIG")
    LINK_HASH_BEFORE_FAIL=$(hash_file "$ARTIFACT_LINK")
    run_case "ART-09-failed-add" 1 "错误密码添加失败" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$SECOND_SOURCE" failed.txt
    assert_values_equal "ART-09-count" "失败添加不改变密文数量" "1" "$(count_enc_files "$ARTIFACT_WS")"
    assert_values_equal "ART-09-meta" "失败添加不改变元数据字节" \
        "$META_HASH_BEFORE_FAIL" "$(hash_file "$ARTIFACT_META")"
    assert_values_equal "ART-09-config" "失败添加不改变全局配置字节" \
        "$CONFIG_HASH_BEFORE_FAIL" "$(hash_file "$ARTIFACT_CONFIG")"
    assert_values_equal "ART-09-link" "失败添加不改变链接字节" \
        "$LINK_HASH_BEFORE_FAIL" "$(hash_file "$ARTIFACT_LINK")"

    EMPTY_SOURCE="$RUN_ROOT/artifact-empty.bin"
    : > "$EMPTY_SOURCE"
    run_case "ART-02-empty" 0 "添加空文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$EMPTY_SOURCE" empty.bin
    assert_values_equal "ART-02-count" "空文件添加后密文数量正确" "2" "$(count_enc_files "$ARTIFACT_WS")"
    # 遍历所有密文长度取最小值，预期最小项来自空文件；不按元数据直接定位 empty.bin 的密文。
    EMPTY_ENC_SIZE=$(for encrypted in "$ARTIFACT_WS"/*.enc; do file_size "$encrypted"; done | sort -n | head -n 1)
    assert_values_equal "ART-02-tag" "空文件密文大小等于 16 字节认证标签" "16" "$EMPTY_ENC_SIZE"

    run_case "ART-06-add-second" 0 "添加第二个文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$SECOND_SOURCE" second.txt
    assert_values_equal "ART-06-second-count" "第二个文件添加后密文数量正确" "3" "$(count_enc_files "$ARTIFACT_WS")"

    MANY_DIR="$RUN_ROOT/artifact-many"
    mkdir -p "$MANY_DIR"
    index=1
    # 每轮创建一个带公开内容标记的普通文件，五项组成目录导入的数量基准。
    while [ "$index" -le 5 ]; do
        printf '%s many-%s\n' "$ARTIFACT_SECRET" "$index" > "$MANY_DIR/file-$index.txt"
        index=$((index + 1))
    done
    run_case "ART-05-add-directory" 0 "添加包含 5 个文件的目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$MANY_DIR" many
    assert_values_equal "ART-05-count" "目录添加后密文数量按 N 增加" "8" "$(count_enc_files "$ARTIFACT_WS")"

    run_case "ART-07-replace" 0 "用相同目标替换文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$REPLACEMENT_SOURCE" first.txt
    assert_values_equal "ART-07-count" "替换不增加活动密文数量" "8" "$(count_enc_files "$ARTIFACT_WS")"
    run_case "ART-07-export" 0 "导出替换后的文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex artifact-check \
        first.txt "$RUN_ROOT/artifact-replaced.txt"
    assert_file_equals "ART-07-bytes" "替换后导出内容正确" \
        "$RUN_ROOT/artifact-replaced.txt" "$REPLACEMENT_SOURCE"

    run_case "ART-08-delete" 0 "删除第二个文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" rm artifact-check second.txt
    assert_values_equal "ART-08-count" "删除后活动密文数量减少一" "7" "$(count_enc_files "$ARTIFACT_WS")"

    # 合计文件内容长度而非文件系统分配块数，再从中文 info 输出中提取数字比较，依赖固定文案格式。
    ACTUAL_BYTES=$(work_file_bytes "$ARTIFACT_WS")
    printf '[实际占用] 工作区字节数: %s\n' "$ACTUAL_BYTES"
    run_case "ART-16-info" 0 "查看容器实际占用" \
        env VEIL_PASSWORD=1 VEIL_LANG=zh VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" info artifact-check
    REPORTED_BYTES=$(sed -n 's/.*容器大小: \([0-9][0-9]*\).*/\1/p' "$RUN_ROOT/cases/ART-16-info.stdout" | head -n 1)
    assert_values_equal "ART-16-size" "info 实际占用等于工作区文件总字节数" \
        "$ACTUAL_BYTES" "$REPORTED_BYTES"
    FIRST_ENC_AFTER=$(find "$ARTIFACT_WS" -maxdepth 1 -type f -name '*.enc' | head -n 1)
    assert_private_mode "DATA-26-enc-mode" "密文仅当前用户可读写" "$FIRST_ENC_AFTER"
    TMP_COUNT=$(find "$ARTIFACT_WS" "$PHASE_HOME/.veil" -type f \( -name '*.tmp' -o -name '*.bak' -o -name '.veil-pw-*' \) 2>/dev/null | wc -l | tr -d ' ')
    assert_values_equal "ART-10-temp" "正常操作后无临时或备份文件" "0" "$TMP_COUNT"

    PACKAGE="$RUN_ROOT/artifact-check.veil"
    run_case "ART-11-pack" 0 "打包产物检查容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" pack artifact-check \
        --output "$PACKAGE"
    printf 'VEILPKG' > "$RUN_ROOT/expected-package-magic.bin"
    dd if="$PACKAGE" of="$RUN_ROOT/actual-package-magic.bin" bs=1 count=7 2>/dev/null
    assert_file_equals "ART-11-magic" "包魔数正确" \
        "$RUN_ROOT/actual-package-magic.bin" "$RUN_ROOT/expected-package-magic.bin"
    # 从固定偏移读取包头中的元数据长度和文件数；od 按本机字节序解析，依赖样本格式与当前主机一致。
    PACKAGE_META_LEN=$(dd if="$PACKAGE" bs=1 skip=14 count=4 2>/dev/null | od -An -tu4 | tr -d ' \n')
    dd if="$PACKAGE" of="$RUN_ROOT/package-meta.bin" bs=1 skip=22 count="$PACKAGE_META_LEN" 2>/dev/null
    assert_file_equals "ART-12-meta-bytes" "包内元数据与工作区元数据逐字节一致" \
        "$RUN_ROOT/package-meta.bin" "$ARTIFACT_META"
    PACKAGE_FILE_COUNT=$(dd if="$PACKAGE" bs=1 skip=18 count=4 2>/dev/null | od -An -tu4 | tr -d ' \n')
    assert_values_equal "ART-11-count" "包头文件数量与活动元数据条目一致" "7" "$PACKAGE_FILE_COUNT"
    PACKAGE_STRINGS="$RUN_ROOT/cases/ART-13-package-strings.txt"
    strings "$PACKAGE" > "$PACKAGE_STRINGS"
    assert_contains "ART-13-name" "包内可见预期的逻辑路径" "first.txt" "$PACKAGE_STRINGS"
    assert_contains "ART-13-dir" "包内可见目录导入路径" "many/file-1.txt" "$PACKAGE_STRINGS"
    assert_true "ART-13-ciphertext" "包内每个密文条目与工作区 .enc 逐字节一致" \
        package_entries_match_work "$PACKAGE" "$ARTIFACT_WS" "$PACKAGE_FILE_COUNT"
    assert_not_contains "ART-04-package-secret" "包内不含明文内容标记" "$ARTIFACT_SECRET" "$PACKAGE"

    CURRENT_LINK_HASH=$(hash_file "$ARTIFACT_LINK")
    assert_values_equal "CONF-11-link-hash" "文件操作不改变链接哈希" "$LINK_HASH_BEFORE" "$CURRENT_LINK_HASH"

    # 改密前后比较元数据变化、链接及稳定 ID 不变，再分别验证旧口令失败和新口令成功，最后恢复测试口令。
    META_HASH_BEFORE_ROTATE=$(hash_file "$ARTIFACT_META")
    CONFIG_ID_BEFORE_ROTATE=$(jsonc_tool get "$ARTIFACT_CONFIG" "/veils/$LINK_ID/veil_id")
    run_case "CONF-12-rotate" 0 "修改密码以检查三个文件变化" \
        env VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=2 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd artifact-check
    assert_values_not_equal "CONF-12-meta-change" "改密后元数据字节发生变化" \
        "$META_HASH_BEFORE_ROTATE" "$(hash_file "$ARTIFACT_META")"
    assert_values_equal "CONF-12-link-stable" "改密后链接字节保持不变" \
        "$CURRENT_LINK_HASH" "$(hash_file "$ARTIFACT_LINK")"
    CONFIG_ID_AFTER_ROTATE=$(jsonc_tool get "$ARTIFACT_CONFIG" "/veils/$LINK_ID/veil_id")
    assert_values_equal "CONF-12-id-stable" "改密后配置稳定 ID 保持不变" \
        "$CONFIG_ID_BEFORE_ROTATE" "$CONFIG_ID_AFTER_ROTATE"
    run_case "CONF-12-old-fails" 1 "改密后旧密码不能读取" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list artifact-check
    run_case "CONF-12-new-works" 0 "改密后新密码可以读取" \
        env VEIL_PASSWORD=2 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list artifact-check
    run_case "CONF-12-rotate-back" 0 "恢复密码 1" \
        env VEIL_PASSWORD=2 VEIL_NEW_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd artifact-check

    printf '\n[配置内容] 全局配置摘录:\n'
    sed -n '1,220p' "$ARTIFACT_CONFIG"
    printf '\n[链接内容] .veil-link:\n'
    cat "$ARTIFACT_LINK"
    printf '\n[元数据] 明文头字符串:\n'
    sed -n '1,80p' "$META_STRINGS"
    printf '\n[产物摘要]\n'
    # 每轮展示一个顶层产物的长度和摘要；这些展示本身不新增断言，也不会验证目录中所有文件类型。
    find "$ARTIFACT_WS" -maxdepth 1 -type f -print | LC_ALL=C sort | while IFS= read -r file; do
        describe_file "$file"
    done
}

# 功能：在 mutate 容器上顺序测试文件/目录移动、删除和口令轮换的退出状态及口令泄露样本。
# 入参：无；依赖 FIXTURES 与 DEBUG_BIN，内部切到 mutate 阶段；返回最后 run_case 状态，失败由全局计数汇总。
# 异常：场景命令失败通常记录后继续；注意：只采集变更前快照，没有在本函数自动比较变更后快照，不能称为完整状态对照验证。
run_mutation_operations_section() {
    section "RM/MV/PASSWD：变更、冲突和密码轮换"
    phase "mutate"

    run_case "MUT-init" 0 "变更测试容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init mutate
    run_case "MUT-add-tree" 0 "准备目录树数据" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add mutate "$FIXTURES/tree"
    run_case "MUT-add-file" 0 "准备普通文件数据" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add mutate "$FIXTURES/hello.txt" keep.txt

    # WS 是此容器密文目录，不是源夹具目录；快照仅记录路径、字节数和摘要供报告观察。
    WS=$(resolve_work_for_link "$PHASE_WORK/mutate.veil-link")
    capture_snapshot "before-mutations" "$WS"

    # 后续路径依赖前面移动成功：keep.txt → renamed.txt → archive/renamed.txt，目录 tree → archive。
    # 每步只按退出码计数，先前步骤失败可能导致后续路径不存在而连带失败。
    run_case "MV-01-file" 0 "重命名文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" mv mutate keep.txt renamed.txt
    run_case "MV-02-directory" 0 "重命名目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" mv mutate tree archive
    run_case "MV-03-into-directory" 0 "移动文件到目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" mv mutate renamed.txt archive
    run_case "MV-04-conflict" 1 "拒绝覆盖已有目标" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" mv mutate archive/top.txt archive/renamed.txt
    run_case "MV-05-cycle" 1 "拒绝把目录移动到自身子目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" mv mutate archive archive/sub
    run_case "MV-06-root" 1 "拒绝移动容器根目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" mv mutate "." new-root
    run_case "MV-07-wrong-password" 1 "错误密码移动失败" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" mv mutate archive/top.txt no.txt

    run_case "RM-01-file" 0 "删除文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" rm mutate archive/renamed.txt
    run_case "RM-02-directory" 0 "递归删除目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" rm mutate ./archive
    run_case "RM-03-missing" 1 "删除不存在的目标失败" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" rm mutate archive
    run_case "RM-04-root" 1 "拒绝删除根目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" rm mutate "."

    run_case "PWD-11-wrong-passwd" 1 "旧密码错误时失败" \
        env VEIL_PASSWORD=wrong VEIL_NEW_PASSWORD=2 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd mutate
    run_case "PWD-03-passwd-empty" 1 "新密码为空时失败" \
        env VEIL_PASSWORD=1 VEIL_NEW_PASSWORD= VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd mutate
    run_case "PWD-13-passwd-old" 0 "改密失败后旧密码仍可用" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list mutate

    # 采集数量和摘要变量但未在本函数用于后续断言；hash_file 是父 Shell 函数，未导出到新 sh -c，摘要采集可能为空。
    # 此前已删除导入的 archive 内容，所以后续轮换也不证明含文件容器的密文保持不变。
    count_before=$(find "$WS" -name '*.enc' | wc -l | tr -d ' ')
    hashes_before=$(find "$WS" -name '*.enc' -exec sh -c 'for f do hash_file "$f"; done' sh {} + 2>/dev/null | sort)
    run_case "PWD-14-rotate" 0 "轮换密码" \
        env VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=2 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd mutate
    run_case "PWD-14-old-fails" 1 "轮换后旧密码失效" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list mutate
    run_case "PWD-14-new-works" 0 "轮换后新密码可用" \
        env VEIL_PASSWORD=2 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list mutate

    run_case "PWD-07-rotate-back" 0 "轮换回密码 1 供后续阶段使用" \
        env VEIL_PASSWORD=2 VEIL_NEW_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd mutate

    run_case "PWD-16-no-leak" 0 "改密时密码不出现在输出中" \
        env VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=secret-rotate-value VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd mutate
    # 搜索固定测试口令，仅覆盖本次输出和 config.jsonc；grep 读取错误也会落入未发现分支，不能视为全面秘密扫描。
    if grep -Fq 'secret-rotate-value' "$RUN_ROOT/cases/PWD-16-no-leak.stdout" "$RUN_ROOT/cases/PWD-16-no-leak.stderr" "$PHASE_HOME/.veil/config.jsonc"; then
        printf '[断言失败] PWD-16 密码泄露到可观察输出或配置中\n'
        ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    else
        printf '[断言通过] PWD-16 输出和配置中未发现密码\n'
    fi
    CHECK_COUNT=$((CHECK_COUNT + 1))
    run_case "PWD-16-rotate-final" 0 "恢复密码 1" \
        env VEIL_PASSWORD=secret-rotate-value VEIL_NEW_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd mutate
}

# 功能：验证包生成、跨隔离配置解包和若干畸形包拒绝场景。
# 入参：无；依赖 DEBUG_BIN 和 FIXTURES，内部切换 pack 阶段并建立额外解包 HOME。
# 返回：场景/断言更新全局计数，函数状态来自最后缺失文件断言；不等于整组是否成功。
# 异常：多数命令失败登记后继续；注意：固定偏移按当前样本格式修改，拒绝只按退出码验证，未对全部失败场景检查所有残留或原有目标内容。
run_pack_unpack_section() {
    section "PACK/UNPACK：包布局、迁移和畸形输入"
    phase "pack"

    run_case "PACK-01-empty-init" 0 "空包源容器初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init empty-pack
    run_case "PACK-01-empty" 0 "打包空容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" pack empty-pack
    assert_file_exists "PACK-01-file" "空包文件存在" "$PHASE_WORK/empty-pack.vault.veil"

    run_case "PACK-02-init" 0 "包生命周期测试容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init package
    run_case "PACK-02-add-text" 0 "向包源容器添加文本" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add package "$FIXTURES/hello.txt" payload.txt
    run_case "PACK-02-add-binary" 0 "向包源容器添加二进制" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add package "$FIXTURES/binary.bin" dir/binary.bin
    run_case "PACK-02-add-unicode" 0 "向包源容器添加 Unicode 名称" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add package \
        "$FIXTURES/中文 目录/子目录/文字 文件.txt" "中文 目录/文字 文件.txt"

    run_case "PACK-04-custom" 0 "打包到自定义路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" pack package \
        --output "$RUN_ROOT/custom-package.veil"
    assert_file_exists "PACK-04-file" "自定义包文件存在" "$RUN_ROOT/custom-package.veil"
    run_case "PACK-05-missing-parent" 0 "打包时创建缺失父目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" pack package \
        --output "$RUN_ROOT/pack-parent/nested/package.veil"
    assert_file_exists "PACK-05-file" "嵌套目录中的包文件存在" "$RUN_ROOT/pack-parent/nested/package.veil"
    run_case "PACK-06-existing" 1 "打包拒绝覆盖已有文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" pack package \
        --output "$RUN_ROOT/custom-package.veil"
    run_case "PACK-07-wrong-password" 1 "错误密码打包失败" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" pack package \
        --output "$RUN_ROOT/wrong-password.veil"
    assert_file_missing "PACK-07-file" "错误密码打包不应留下文件" "$RUN_ROOT/wrong-password.veil"

    PACKAGE="$RUN_ROOT/custom-package.veil"
    # 多个独立 HOME 隔离容器 ID 注册，允许相同包在不同配置恢复；链接仍写到本阶段工作目录。
    UNPACK_01_HOME="$RUN_ROOT/home-pack-unpack-01"
    UNPACK_03_HOME="$RUN_ROOT/home-pack-unpack-03"
    UNPACK_04_HOME="$RUN_ROOT/home-pack-unpack-04"
    UNPACK_FAIL_HOME="$RUN_ROOT/home-pack-unpack-fail"
    mkdir -p "$UNPACK_01_HOME" "$UNPACK_03_HOME" "$UNPACK_04_HOME" "$UNPACK_FAIL_HOME"
    # 头字节和 strings 结果仅打印观察；这里未据输出判断包格式或路径是否正确。
    printf '\n[格式] 包头部和可见路径\n'
    dd if="$PACKAGE" bs=1 count=22 2>/dev/null | od -An -tx1 -v
    printf '魔数: '
    dd if="$PACKAGE" bs=1 count=8 2>/dev/null
    printf '\n包内可见的载荷名称（用于确认当前格式行为）:\n'
    strings "$PACKAGE" | grep -E 'payload\.txt|binary\.bin|文字' || true

    run_case "UNPACK-01" 0 "解包到默认工作区" \
        env HOME="$UNPACK_01_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name restored
    assert_file_exists "UNPACK-01-link" "解包后的链接存在" "$PHASE_WORK/restored.veil-link"
    run_case "UNPACK-02-export" 0 "解包后导出文件" \
        env HOME="$UNPACK_01_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex restored payload.txt "$RUN_ROOT/restored-payload.txt"
    assert_file_equals "UNPACK-02-bytes" "解包后的内容一致" \
        "$RUN_ROOT/restored-payload.txt" "$FIXTURES/hello.txt"
    run_case "UNPACK-03-name-extract" 0 "从包文件名推导容器名" \
        env HOME="$UNPACK_03_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack \
        "$RUN_ROOT/pack-parent/nested/package.veil" --link "$PHASE_WORK/name-extract.veil-link"
    run_case "UNPACK-04-custom-name" 0 "解包时指定展示名称" \
        env HOME="$UNPACK_04_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name custom-name --link "$PHASE_WORK/custom-unpack.veil-link"
    run_case "UNPACK-04-duplicate-id" 1 "同一配置重复解包拒绝 ID 冲突" \
        env HOME="$UNPACK_04_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name duplicate-id --link "$PHASE_WORK/duplicate-id.veil-link"
    run_case "UNPACK-06-missing-work" 1 "解包时拒绝未知命名工作区" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name work-missing --work no-such-work
    run_case "UNPACK-08-bad-extension" 1 "解包时检查链接扩展名" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name bad-link --link "$PHASE_WORK/bad-unpack.txt"
    run_case "UNPACK-09-existing-link" 1 "解包拒绝覆盖已有链接" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name existing --link "$PHASE_WORK/restored.veil-link"

    run_case "UNPACK-11-wrong-password" 1 "错误密码在创建产物前失败" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name wrong-unpack --link "$PHASE_WORK/wrong-unpack.veil-link"
    assert_file_missing "UNPACK-11-link" "错误密码解包不应留下链接" "$PHASE_WORK/wrong-unpack.veil-link"

    printf '\n[完整性] 畸形包副本\n'
    # 后续每项都从原包独立复制再损坏一个字段，避免前一项损坏影响下一项；原包保留供其他用例使用。
    trunc="$RUN_ROOT/truncated.veil"
    dd if="$PACKAGE" of="$trunc" bs=1 count=10 2>/dev/null
    run_case "UNPACK-12-truncated" 1 "拒绝截断的包" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$trunc" \
        --name trunc --link "$PHASE_WORK/trunc.veil-link"

    bad_magic="$RUN_ROOT/bad-magic.veil"
    cp "$PACKAGE" "$bad_magic"
    printf 'X' | dd of="$bad_magic" bs=1 seek=0 conv=notrunc 2>/dev/null
    run_case "UNPACK-12-magic" 1 "拒绝错误魔数" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_magic" \
        --name magic --link "$PHASE_WORK/magic.veil-link"

    # 在版本字段写入两字节值 2；位置由脚本旧格式约定确定，不是从包结构动态解析。
    bad_version="$RUN_ROOT/bad-version.veil"
    cp "$PACKAGE" "$bad_version"
    printf '\002\000' | dd of="$bad_version" bs=1 seek=8 conv=notrunc 2>/dev/null
    run_case "UNPACK-13-version" 1 "拒绝不支持的包版本" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_version" \
        --name version --link "$PHASE_WORK/version.veil-link"

    bad_header_size="$RUN_ROOT/bad-header-size.veil"
    cp "$PACKAGE" "$bad_header_size"
    printf '\027\000\000\000' | dd of="$bad_header_size" bs=1 seek=10 conv=notrunc 2>/dev/null
    run_case "UNPACK-13-header-size" 1 "拒绝不支持的包头长度" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_header_size" \
        --name header-size --link "$PHASE_WORK/header-size.veil-link"

    bad_meta_len="$RUN_ROOT/bad-meta-len.veil"
    cp "$PACKAGE" "$bad_meta_len"
    printf '\000\000\000\004' | dd of="$bad_meta_len" bs=1 seek=14 conv=notrunc 2>/dev/null
    run_case "UNPACK-14-meta-len" 1 "拒绝超过文件的元数据长度" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_meta_len" \
        --name meta-len --link "$PHASE_WORK/meta-len.veil-link"

    # 将头部条目数改为零或最大 32 位值，制造与元数据/实际载荷不一致，检查解包是否拒绝。
    bad_count_low="$RUN_ROOT/bad-count-low.veil"
    cp "$PACKAGE" "$bad_count_low"
    printf '\000\000\000\000' | dd of="$bad_count_low" bs=1 seek=18 conv=notrunc 2>/dev/null
    run_case "UNPACK-15-count-low" 1 "拒绝少于元数据声明的文件数" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_count_low" \
        --name count-low --link "$PHASE_WORK/count-low.veil-link"

    bad_count_high="$RUN_ROOT/bad-count-high.veil"
    cp "$PACKAGE" "$bad_count_high"
    printf '\377\377\377\377' | dd of="$bad_count_high" bs=1 seek=18 conv=notrunc 2>/dev/null
    run_case "UNPACK-16-count-high" 1 "拒绝多于元数据声明的文件数" \
        env HOME="$UNPACK_FAIL_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_count_high" \
        --name count-high --link "$PHASE_WORK/count-high.veil-link"

    # 小端 0x40000000 实际声明 1 GiB 元数据；已有用例描述写作 64 MiB，并不准确。
    # 这里只要求返回 1，没有设置内存上限或监控分配量，不能独立证明有界内存使用。
    oversized_meta="$RUN_ROOT/oversized-meta.veil"
    cp "$PACKAGE" "$oversized_meta"
    printf '\000\000\000\100' | dd of="$oversized_meta" bs=1 seek=14 conv=notrunc 2>/dev/null
    run_shell_case "UNPACK-22-bounded-allocation" 1 \
        "声明 64 MiB 元数据时应有界失败" \
        "HOME='$UNPACK_FAIL_HOME' VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$oversized_meta' --name oversized --link '$PHASE_WORK/oversized.veil-link'"

    # 删除源容器首个密文再请求打包，检查缺文件失败及输出包不存在；故意破坏的是本组测试容器。
    # xargs 未采用 NUL 分隔，路径含空格等字符时可能无法精确删除目标。
    PACK_WS=$(resolve_work_for_link "$PHASE_WORK/package.veil-link")
    run_case "PACK-08-missing-ciphertext" 1 "密文缺失时打包失败" \
        sh -c "find '$PACK_WS' -name '*.enc' | head -n 1 | xargs rm -f; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack package --output '$RUN_ROOT/missing-ciphertext.veil'"
    assert_file_missing "PACK-08-output" "密文缺失时不应留下包文件" "$RUN_ROOT/missing-ciphertext.veil"
}

# 功能：检查提示级别配置、链接复制/恢复、非法缓存字段与损坏配置的拒绝行为。
# 入参：无；依赖前面打包场景生成的 custom-package.veil，切到 config-link 隔离阶段。
# 返回：更新场景和断言计数，函数状态取决于最后并发或跳过分支；不作为整组通过标志。
# 异常：jsonc_tool 修改失败显式 return 1，其他多数错误记录后继续；注意：硬编码 /links/1 假设第二个缓存项是目标链接，顺序变化会使样本失真。
run_config_link_section() {
    section "CFG/LINK/RES：配置、链接恢复和篡改处理"
    phase "config-link"
    REC_11_HOME="$RUN_ROOT/home-detached"
    mkdir -p "$REC_11_HOME"

    run_case "CFG-01" 0 "无配置文件的配置查看" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" config
    assert_file_missing "CFG-01-no-side-effect" "查看配置不应创建配置文件" "$PHASE_HOME/.veil/config.jsonc"
    run_case "CFG-02" 0 "显式 config show" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" config show
    run_case "CFG-05-full" 0 "设置完整提示级别" \
        env VEIL_HINTS= VEIL_TEST_KDF=fast "$DEBUG_BIN" config --hints full
    run_case "CFG-05-brief" 0 "设置简要提示级别" \
        env VEIL_HINTS= VEIL_TEST_KDF=fast "$DEBUG_BIN" config --hints brief
    run_case "CFG-05-off" 0 "关闭提示级别" \
        env VEIL_HINTS= VEIL_TEST_KDF=fast "$DEBUG_BIN" config --hints off
    run_case "CFG-07-invalid" 1 "拒绝非法提示级别" \
        env VEIL_HINTS= VEIL_TEST_KDF=fast "$DEBUG_BIN" config --hints invalid
    run_case "CFG-06-case-space" 0 "提示级别忽略大小写和首尾空格" \
        env VEIL_HINTS= VEIL_TEST_KDF=fast "$DEBUG_BIN" config --hints '  FULL  '
    run_case "CFG-08-env-override" 0 "配置显示环境变量覆盖" \
        env VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" config show
    run_case "CFG-09-persist-override" 0 "环境覆盖期间仍持久化新的提示级别" \
        env VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" config --hints off

    run_case "LINK-init" 0 "链接测试容器初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init linked
    run_case "LINK-add" 0 "准备链接测试数据" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add linked "$FIXTURES/hello.txt"
    run_case "LINK-01-secondary" 0 "创建第二个链接" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link "$PHASE_WORK/linked.veil-link" --output "$PHASE_WORK/second.veil-link"
    assert_file_equals "LINK-01-bytes" "第二个链接与原链接逐字节一致" \
        "$PHASE_WORK/linked.veil-link" "$PHASE_WORK/second.veil-link"

    # 先备份第二份链接再删掉磁盘文件，随后用缓存恢复；比较 JSON 字段值，而非要求恢复后的排版字节完全一致。
    cp "$PHASE_WORK/second.veil-link" "$RUN_ROOT/link-original.veil-link"
    rm "$PHASE_WORK/second.veil-link"
    run_case "RES-13-restore" 0 "从缓存正文重新生成已删除链接" \
        env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" list second.veil-link
    assert_values_equal "RES-13-body" "重建后的链接与原正文全部字段值一致" \
        "$(jsonc_tool get "$PHASE_WORK/second.veil-link" '')" \
        "$(jsonc_tool get "$RUN_ROOT/link-original.veil-link" '')"

    # 每组先备份有效配置，再把缓存链接中的 ID 改为路径穿越或空值并删除链接，测试恢复前的输入校验。
    # 失败后用备份覆盖恢复配置；未用 trap 保护，若辅助程序提前 return，恢复步骤可能不会执行。
    cp "$PHASE_HOME/.veil/config.jsonc" "$RUN_ROOT/config-before-corruption.jsonc"
    jsonc_tool set "$PHASE_HOME/.veil/config.jsonc" /links/1/link/veil/veil_id ../outside || return 1
    rm -f "$PHASE_WORK/second.veil-link"
    run_case "RES-15-unsafe-path" 1 "拒绝缓存正文中的非法容器 ID" \
        env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" list second.veil-link
    assert_true "RES-15-no-file" "无效路径不会写出链接文件" test ! -e "$PHASE_WORK/second.veil-link"
    cp "$RUN_ROOT/config-before-corruption.jsonc" "$PHASE_HOME/.veil/config.jsonc"

    cp "$PHASE_HOME/.veil/config.jsonc" "$RUN_ROOT/config-before-invalid-id.jsonc"
    jsonc_tool set "$PHASE_HOME/.veil/config.jsonc" /links/1/link/veil/veil_id '' || return 1
    rm -f "$PHASE_WORK/second.veil-link"
    run_case "RES-14-empty-id" 1 "拒绝缓存正文中的空容器 ID" \
        env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" list second.veil-link
    assert_true "RES-14-no-file" "空容器 ID 不会写出链接文件" test ! -e "$PHASE_WORK/second.veil-link"
    cp "$RUN_ROOT/config-before-invalid-id.jsonc" "$PHASE_HOME/.veil/config.jsonc"
    run_case "RES-13-restore-again" 0 "修复配置后恢复链接" \
        env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" list second.veil-link

    run_case "LINK-02-new-link" 0 "根据配置重新生成链接" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link linked --output "$PHASE_WORK/generated.veil-link"
    run_case "LINK-07-existing" 1 "拒绝覆盖已有链接输出" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link linked --output "$PHASE_WORK/generated.veil-link"
    run_case "LINK-09-extension" 1 "检查链接输出扩展名" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link linked --output "$PHASE_WORK/generated.txt"
    run_case "LINK-08-parent" 0 "创建链接父目录" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link linked --output "$RUN_ROOT/link-parent/nested/new.veil-link"

    run_case "RES-06-work-path" 1 "普通命令拒绝直接使用工作区路径" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$(resolve_work_for_link "$PHASE_WORK/linked.veil-link")"
    run_case "RES-07-ordinary-directory" 1 "拒绝普通目录作为容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$FIXTURES"
    run_case "RES-08-package-direct" 1 "直接访问包文件时提示先解包" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/custom-package.veil"

    run_case "RES-28-invalid-link" 1 "拒绝格式损坏的链接" \
        sh -c "printf 'not jsonc\\n' > '$PHASE_WORK/broken.veil-link'; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list '$PHASE_WORK/broken.veil-link'"

    # 文件名虽叫 absolute，实际修改的是 veil_id 为未知值，没有在链接中放入绝对工作区路径。
    cp "$PHASE_WORK/linked.veil-link" "$PHASE_WORK/absolute.veil-link"
    ws=$(resolve_work_for_link "$PHASE_WORK/linked.veil-link")
    jsonc_tool set "$PHASE_WORK/absolute.veil-link" /veil/veil_id veil-missing || return 1
    run_case "RES-29-absolute-link-path" 1 "链接中的未知容器 ID 不按名称回退" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$PHASE_WORK/absolute.veil-link"

    # 独立破坏整份 JSONC 语法，仅验证 config show 返回失败，再恢复原配置。
    cp "$PHASE_HOME/.veil/config.jsonc" "$RUN_ROOT/config-good.jsonc"
    printf 'this is not valid JSONC\n' > "$PHASE_HOME/.veil/config.jsonc"
    run_case "CFG-10-corrupt" 1 "拒绝损坏的配置文件" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" config show
    cp "$RUN_ROOT/config-good.jsonc" "$PHASE_HOME/.veil/config.jsonc"

    printf '\n[配置] 最终配置:\n'
    sed -n '1,260p' "$PHASE_HOME/.veil/config.jsonc"

    if [ "$RUN_STRESS" -eq 1 ]; then
        # 双进程同时设置不同提示级别，仅展示结果和最终配置尾部；run_pair 不按双方退出码或最终配置正确性判失败。
        run_pair "CFG-17-parallel" "并发写入提示级别配置" \
            "VEIL_HINTS= VEIL_TEST_KDF=fast '$DEBUG_BIN' config --hints full" \
            "VEIL_HINTS= VEIL_TEST_KDF=fast '$DEBUG_BIN' config --hints off"
        printf '[观察] 并发写入后的配置尾部:\n'
        tail -n 40 "$PHASE_HOME/.veil/config.jsonc"
    else
        skip_case "CFG-17" "并发测试未启用"
    fi
}

# 功能：通过管道向 CLI shell 命令循环输入固定脚本，并可选执行真实终端密码输入。
# 入参：无；使用 FIXTURES、DEBUG_BIN 和 RUN_TTY，切换 shell 阶段；返回最后交互或跳过分支状态，结果累计到全局计数。
# 异常：多数命令错误按退出状态登记；注意：自动部分不是伪终端，不能验证密码不回显；Shell 内部单条命令失败后会话仍可能退出 0。
run_shell_section() {
    section "SHELL：交互式命令循环"
    phase "shell"

    run_case "SHELL-init" 0 "shell 测试容器初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init shellbox
    run_case "SHELL-01-help-exit" 0 "shell 帮助和退出" \
        sh -c "printf 'help\\nexit\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    run_case "SHELL-02-empty-list" 0 "shell 空列表" \
        sh -c "printf 'ls\\nexit\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    run_case "SHELL-03-add-list" 0 "shell 添加并列出文件" \
        sh -c "printf 'add $FIXTURES/hello.txt\\nls\\nexit\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    run_case "SHELL-04-export" 0 "shell 导出文件" \
        sh -c "printf 'ex hello.txt $RUN_ROOT/shell-export.txt\\nexit\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    assert_file_equals "SHELL-04-bytes" "shell 导出内容与源文件一致" "$RUN_ROOT/shell-export.txt" "$FIXTURES/hello.txt"
    run_case "SHELL-05-rm" 0 "shell 删除文件" \
        sh -c "printf 'rm hello.txt\\nls\\nexit\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    run_case "SHELL-06-unknown" 0 "shell 未知命令后会话继续" \
        sh -c "printf 'unknown command\\nquit\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    run_case "SHELL-07-wrong-password" 1 "shell 错误密码不能进入" \
        sh -c "printf 'exit\\n' | VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    run_case "SHELL-08-uppercase" 0 "观察 shell 命令大小写敏感性" \
        sh -c "printf 'LS\\nexit\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"
    # 未给 CLI shell 内的含空格路径加专门转义；用例只观察会话退出码，不断言该文件真的导入成功。
    run_case "SHELL-09-spaced-path" 0 "shell 含空格路径的处理保持一致" \
        sh -c "printf 'add $FIXTURES/name with spaces & symbols (1).txt\\nls\\nq\\n' | VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' shell shellbox"

    if [ "$RUN_TTY" -eq 1 ]; then
        run_interactive_case "PWD-01-tty-init" 0 \
            "交互式密码不回显并二次确认（输入两次 1）" \
            env -u VEIL_PASSWORD VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init tty-pass
        run_interactive_observation "PWD-02-tty-mismatch" 1 \
            "交互式密码两次不一致（先输入 1，再输入 2）" \
            env -u VEIL_PASSWORD VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init tty-mismatch
    else
        skip_case "PWD-01" "交互式 TTY 未启用"
        skip_case "PWD-02" "交互式 TTY 未启用"
    fi
}

# 功能：在专用 corrupt 测试容器中制造元数据和密文损坏，再检查拒绝、恢复及重建链接。
# 入参：无；依赖 DEBUG_BIN、FIXTURES 和前一配置场景设置的 REC_11_HOME，切换 corruption 阶段。
# 返回：各命令/断言累计结果，函数状态取最后 run_case（通常 0）；不自动汇总为成功/失败返回值。
# 异常：多数操作失败后继续；注意：直接修改有效样本后用备份恢复，不是始终只改副本；没有恢复 trap，中途终止可能留下故意损坏的测试容器。
run_corruption_section() {
    section "DATA/REC：元数据、密文损坏和恢复"
    phase "corruption"

    run_case "DATA-init" 0 "损坏测试容器初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init corrupt
    run_case "DATA-add-good" 0 "添加正常文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add corrupt "$FIXTURES/hello.txt" good.txt
    WS=$(resolve_work_for_link "$PHASE_WORK/corrupt.veil-link")
    # 先备份完整元数据和摘要；每个损坏场景结束后手动覆盖恢复，最终比较是否回到原始字节。
    meta="$WS/.veil-info"
    cp "$meta" "$RUN_ROOT/meta-good.bin"
    meta_before=$(hash_file "$meta")

    # 用例名叫 bitflip，但实际把末字节写为固定 0x01；若原值恰好为 0x01，字节可能未改变。
    run_case "DATA-13-metadata-bitflip" 1 "拒绝被篡改的元数据" \
        sh -c "cp '$RUN_ROOT/meta-good.bin' '$meta'; size=\$(wc -c < '$meta' | tr -d ' '); seek=\$((size - 1)); printf '\\001' | dd of='$meta' bs=1 seek=\$seek conv=notrunc 2>/dev/null; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list corrupt"
    cp "$RUN_ROOT/meta-good.bin" "$meta"
    run_case "DATA-04-metadata-truncate" 1 "拒绝截断的元数据" \
        sh -c "cp '$RUN_ROOT/meta-good.bin' '$meta'; size=\$(wc -c < '$meta' | tr -d ' '); new=\$((size / 2)); dd if='$RUN_ROOT/meta-good.bin' of='$meta' bs=1 count=\$new 2>/dev/null; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list corrupt"
    cp "$RUN_ROOT/meta-good.bin" "$meta"
    run_case "DATA-05-metadata-extra" 1 "拒绝多一字节的元数据" \
        sh -c "cp '$RUN_ROOT/meta-good.bin' '$meta'; printf 'X' >> '$meta'; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list corrupt"
    cp "$RUN_ROOT/meta-good.bin" "$meta"
    assert_true "DATA-05-meta-restored" "metadata hash restored" test "$(hash_file "$meta")" = "$meta_before"

    # 只有一个导入文件时取排序后的首个密文作为 good.txt 样本；另存原字节供每次恢复。
    first_enc=$(find "$WS" -name '*.enc' | LC_ALL=C sort | head -n 1)
    cp "$first_enc" "$RUN_ROOT/enc-good.bin"
    run_case "DATA-13-cipher-bitflip" 1 "拒绝被篡改的密文" \
        sh -c '
            source=$1
            target=$2
            output=$3
            binary=$4
            cp "$source" "$target"
            original=$(dd if="$target" bs=1 count=1 2>/dev/null | od -An -tu1 | tr -d " ")
            flipped=$((original ^ 1))
            printf "\\$(printf "%03o" "$flipped")" \
                | dd of="$target" bs=1 seek=0 conv=notrunc 2>/dev/null
            VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast \
                "$binary" ex corrupt good.txt "$output"
        ' sh "$RUN_ROOT/enc-good.bin" "$first_enc" "$RUN_ROOT/corrupt-export.bin" "$DEBUG_BIN"
    assert_file_missing "DATA-13-no-output" "corrupt ciphertext export leaves no output" "$RUN_ROOT/corrupt-export.bin"
    cp "$RUN_ROOT/enc-good.bin" "$first_enc"
    run_case "DATA-15-cipher-truncate" 1 "拒绝截断的密文" \
        sh -c "cp '$RUN_ROOT/enc-good.bin' '$first_enc'; size=\$(wc -c < '$RUN_ROOT/enc-good.bin' | tr -d ' '); new=\$((size - 1)); dd if='$RUN_ROOT/enc-good.bin' of='$first_enc' bs=1 count=\$new 2>/dev/null; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex corrupt good.txt '$RUN_ROOT/truncated-export.bin'"
    cp "$RUN_ROOT/enc-good.bin" "$first_enc"

    run_case "DATA-16-missing-cipher" 1 "密文缺失时导出失败" \
        sh -c "cp '$first_enc' '$RUN_ROOT/enc-backup.bin'; rm '$first_enc'; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex corrupt good.txt '$RUN_ROOT/missing-cipher-export.bin'; rc=\$?; mv '$RUN_ROOT/enc-backup.bin' '$first_enc'; exit \$rc"

    # 临时移走阶段配置再读取链接，记录 CLI 状态后移回；恢复 mv 的失败未另行断言。
    run_case "REC-10-lost-config" 1 "配置丢失且无已登记工作区时报告未找到" \
        sh -c "mv '$PHASE_HOME/.veil/config.jsonc' '$RUN_ROOT/config-lost.jsonc'; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list '$PHASE_WORK/corrupt.veil-link'; rc=\$?; mv '$RUN_ROOT/config-lost.jsonc' '$PHASE_HOME/.veil/config.jsonc'; exit \$rc"
    # 复制密文目录到另一个根路径后重命名根目录，再以独立 HOME 从元数据头重建链接并验证能打开。
    # REC_11_HOME 来自前一场景，本函数不是完全独立的测试入口。
    run_case "REC-11-header-link" 0 "根据工作区元数据头重建链接" \
        sh -c '
            source=$1
            original=$2
            renamed=$3
            recovery_home=$4
            binary=$5
            output=$6
            veil_id=$(basename "$source")
            mkdir -p "$original"
            cp -R "$source" "$original/$veil_id"
            mv "$original" "$renamed"
            env HOME="$recovery_home" VEIL_HINTS=off VEIL_TEST_KDF=fast \
                "$binary" link "$renamed/$veil_id" --output "$output"
        ' sh "$WS" "$RUN_ROOT/detached-work" "$RUN_ROOT/detached-work-renamed" \
            "$REC_11_HOME" "$DEBUG_BIN" "$RUN_ROOT/detached.veil-link"
    run_case "REC-11-open" 0 "打开重建的脱离工作区链接" \
        env HOME="$REC_11_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/detached.veil-link"
}

# 功能：串联位置参数、选项参数、多链接、备份迁移和改密迁移的用户操作链路。
# 入参：无；使用 DEBUG_BIN、FIXTURES，在 e2e 阶段及额外独立 HOME 中生成配置和容器。
# 返回：通过 run_case 和字节比较更新全局计数，最后观察打印通常返回 0，不单独汇总整组状态。
# 异常：组合命令使用 && 在首个失败处停止；注意：期望最终退出 1 的空生命周期无法只靠状态区分“末尾按预期失败”和“前面意外失败”，也没有清理每个已生成容器登记。
run_e2e_section() {
    section "E2E：用户旅程和选项参数等价性"
    phase "e2e"
    # 三个独立配置目录分别接收备份、改密包和空包，避免同一容器 ID 在原配置中重复登记冲突。
    E2E_BACKUP_HOME="$RUN_ROOT/home-e2e-backup"
    E2E_MIGRATE_HOME="$RUN_ROOT/home-e2e-migrate"
    E2E_EMPTY_HOME="$RUN_ROOT/home-e2e-empty"
    mkdir -p "$E2E_BACKUP_HOME" "$E2E_MIGRATE_HOME" "$E2E_EMPTY_HOME"

    run_case "E2E-01-init" 0 "E2E 位置参数形式初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init journey 1
    run_case "E2E-01-add" 0 "E2E 添加文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add journey "$FIXTURES/hello.txt" docs/hello.txt 1
    run_case "E2E-01-list" 0 "E2E 列表" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list journey 1
    run_case "E2E-01-free" 0 "E2E 树形视图" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" free journey 1
    run_case "E2E-01-export" 0 "E2E 导出" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex journey docs/hello.txt "$RUN_ROOT/e2e-export.txt" 1
    assert_file_equals "E2E-01-bytes" "E2E export matches" "$RUN_ROOT/e2e-export.txt" "$FIXTURES/hello.txt"
    run_case "E2E-01-rm" 0 "E2E 删除" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" rm journey docs/hello.txt 1

    run_case "E2E-02-options" 0 "E2E 选项参数形式生命周期" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init option-form --password 1 && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add option-form --input '$FIXTURES/hello.txt' --output option.txt --password 1 && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex option-form --input option.txt --output '$RUN_ROOT/option-export.txt' --password 1"
    assert_file_equals "E2E-02-bytes" "option-form export matches" "$RUN_ROOT/option-export.txt" "$FIXTURES/hello.txt"

    # 通过第二份链接导入，再从原名称查询，观察多个入口是否指向同一容器；没有解析列表正文断言 shared.txt。
    run_case "E2E-03-multi-link" 0 "多链接工作流" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init multilink && VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' link multilink --output '$RUN_ROOT/other-link.veil-link' && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add '$RUN_ROOT/other-link.veil-link' '$FIXTURES/hello.txt' shared.txt && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list multilink"

    # 包先移动到新目录，再在独立 HOME 解包和导出；随后 cmp 验证二进制往返字节一致。
    run_case "E2E-04-backup" 0 "打包、移动、解包、导出备份工作流" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init backup && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add backup '$FIXTURES/binary.bin' && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack backup --output '$RUN_ROOT/backup.veil' && mkdir -p '$RUN_ROOT/moved' && mv '$RUN_ROOT/backup.veil' '$RUN_ROOT/moved/backup.veil' && HOME='$E2E_BACKUP_HOME' VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$RUN_ROOT/moved/backup.veil' --name restored-backup --link '$RUN_ROOT/restored-backup.veil-link' && HOME='$E2E_BACKUP_HOME' VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex '$RUN_ROOT/restored-backup.veil-link' binary.bin '$RUN_ROOT/backup-export.bin'"
    assert_file_equals "E2E-04-bytes" "backup export matches" "$RUN_ROOT/backup-export.bin" "$FIXTURES/binary.bin"

    run_case "E2E-05-password-migration" 0 "改密迁移工作流" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init migrate && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add migrate '$FIXTURES/hello.txt' && VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' passwd migrate && VEIL_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack migrate --output '$RUN_ROOT/migrated.veil' && HOME='$E2E_MIGRATE_HOME' VEIL_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$RUN_ROOT/migrated.veil' --name migrated --link '$RUN_ROOT/migrated.veil-link'"
    run_case "E2E-05-old-fails" 1 "迁移后旧密码失效" \
        env HOME="$E2E_MIGRATE_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/migrated.veil-link"
    run_case "E2E-05-new-works" 0 "新密码可打开迁移后的包" \
        env HOME="$E2E_MIGRATE_HOME" VEIL_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/migrated.veil-link"

    run_case "E2E-10-empty-lifecycle" 1 "空生命周期，末尾删除不存在文件预期失败" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init empty-e2e && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack empty-e2e --output '$RUN_ROOT/empty-e2e.veil' && HOME='$E2E_EMPTY_HOME' VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$RUN_ROOT/empty-e2e.veil' --name empty-e2e-copy --link '$RUN_ROOT/empty-e2e-copy.veil-link' && HOME='$E2E_EMPTY_HOME' VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=4 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' passwd empty-e2e-copy && HOME='$E2E_EMPTY_HOME' VEIL_PASSWORD=4 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' rm empty-e2e-copy missing-file"
    printf '[观察] 空生命周期末尾删除不存在文件应正常失败；组合命令退出码见上方。\n'
}

# 功能：使用 RELEASE_BIN 运行创建、导入、导出、打包、独立配置解包及错误口令检查。
# 入参：无；依赖 ensure_binaries 准备 Release 文件和 FIXTURES，内部切到 release 阶段。
# 返回：更新全局计数，最后扩展或跳过分支状态通常为 0；不是发布目录结构验证。
# 异常：场景失败记录后继续；注意：这里不设置 VEIL_TEST_KDF=fast，口令派生按发布实现执行；--release-full 只额外运行三步改密场景，不表示重复全部 Debug 用例。
run_release_section() {
    section "Release 构建：安全提示和核心生命周期"
    phase "release"
    RELEASE_COPY_HOME="$RUN_ROOT/home-release-copy"
    mkdir -p "$RELEASE_COPY_HOME"

    # 此命令仅按 --version 退出码计数，用例文案虽写“安全提示”，没有断言提示正文。
    run_case "GLB-02" 0 "Release 版本安全提示" \
        env VEIL_HINTS=off "$RELEASE_BIN" --version

    run_case "REL-01-init" 0 "Release 初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" init release-box
    run_case "REL-02-add" 0 "Release 添加文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" add release-box "$FIXTURES/hello.txt" release.txt
    run_case "REL-03-export" 0 "Release 导出" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" ex release-box release.txt "$RUN_ROOT/release-export.txt"
    assert_file_equals "REL-03-bytes" "Release export matches" "$RUN_ROOT/release-export.txt" "$FIXTURES/hello.txt"
    run_case "REL-04-pack" 0 "Release 打包" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" pack release-box --output "$RUN_ROOT/release.veil"
    run_case "REL-05-unpack" 0 "Release 解包" \
        env HOME="$RELEASE_COPY_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" unpack "$RUN_ROOT/release.veil" \
        --name release-copy --link "$PHASE_WORK/release-copy.veil-link"
    run_case "REL-06-export-copy" 0 "Release 导出解包副本" \
        env HOME="$RELEASE_COPY_HOME" VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" ex release-copy release.txt "$RUN_ROOT/release-copy-export.txt"
    assert_file_equals "REL-06-bytes" "Release round-trip matches" \
        "$RUN_ROOT/release-copy-export.txt" "$FIXTURES/hello.txt"

    run_case "REL-07-wrong-password" 1 "Release 错误密码被拒绝" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off "$RELEASE_BIN" list release-box

    if [ "$RELEASE_FULL" -eq 1 ]; then
        run_case "REL-FULL-01" 0 "Release 轮换密码" \
            env VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=2 VEIL_HINTS=off "$RELEASE_BIN" passwd release-box
        run_case "REL-FULL-02" 0 "Release 使用新密码" \
            env VEIL_PASSWORD=2 VEIL_HINTS=off "$RELEASE_BIN" list release-box
        run_case "REL-FULL-03" 0 "Release 轮换回原密码" \
            env VEIL_PASSWORD=2 VEIL_NEW_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" passwd release-box
    else
        skip_case "REL-FULL" "使用 --release-full 可启用扩展 Release 覆盖"
    fi
}

# 功能：在指定目录创建测试链接，观察手动卸载/重挂载后的读取、导出和继续写入。
# 入参：无；读取 RUN_EXTERNAL、EXTERNAL_DIR、EXTERNAL_CYCLES、KEEP_DATA，依赖 DEBUG_BIN 和 FIXTURES。
# 返回：禁用时登记跳过；启用时更新场景/断言计数，末尾按保留策略删除样本。
# 异常：系统命令失败多数未单独终止；注意：不验证路径确为外置卷，不自动卸载挂载，交互检查只以链接存在性推断状态。删除范围包含解析出的 EXTERNAL_WS，可能不在 EXT 内。
run_external_section() {
    if [ "$RUN_EXTERNAL" -ne 1 ]; then
        section "PLAT：外置卷工作流"
        skip_case "INIT-27" "未提供外置卷"
        skip_case "E2E-07" "未提供外置卷"
        return
    fi

    section "PLAT：外置卷工作流"
    phase "external"
    # 时间戳与进程号生成专属样本目录；EXTERNAL_DIR 来自参数或环境，不在此校验卷类型或访问授权。
    EXT="$EXTERNAL_DIR/veil-scenario-$RUN_STAMP-$$"
    mkdir -p "$EXT"
    note "External test directory: $EXT"

    run_case "INIT-27-external" 0 "外置卷初始化自动采用便携布局" \
        env VEIL_PASSWORD=1 VEIL_HINTS=full VEIL_TEST_KDF=fast "$DEBUG_BIN" init external \
        --link "$EXT/external.veil-link"
    assert_file_exists "INIT-27-link" "外置卷链接存在" "$EXT/external.veil-link"
    EXTERNAL_WS=$(resolve_work_for_link "$EXT/external.veil-link")
    assert_file_exists "INIT-27-portable" "外置卷默认工作区存在" "$EXTERNAL_WS"

    run_case "E2E-07-add" 0 "在外置卷添加文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add "$EXT/external.veil-link" \
        "$FIXTURES/hello.txt"
    run_case "E2E-07-export" 0 "从外置卷导出文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex "$EXT/external.veil-link" \
        hello.txt "$RUN_ROOT/external-export.txt"
    assert_file_equals "E2E-07-bytes" "外置卷导出内容一致" \
        "$RUN_ROOT/external-export.txt" "$FIXTURES/hello.txt"

    # 再创建一个位于本地卷的链接副本。卸载外置卷后测试这个本地链接，
    # 避免访问外置卷上的原链接时在 /Volumes 下创建影子目录。
    run_case "RES-27-local-pointer" 0 "创建本地卷上的外置工作区链接" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link "$EXT/external.veil-link" \
        --output "$RUN_ROOT/external-local-pointer.veil-link"
    EXTERNAL_WS=$(resolve_work_for_link "$EXT/external.veil-link")
    EXTERNAL_LINK_HASH_BEFORE=$(hash_file "$RUN_ROOT/external-local-pointer.veil-link")
    EXTERNAL_ENC_BEFORE=$(count_enc_files "$EXTERNAL_WS")

    printf '\n[外置卷] 用户手动卸载前的目录布局:\n'
    find "$EXT" -maxdepth 4 -print | LC_ALL=C sort | sed -n '1,200p'

    if [ "$NON_INTERACTIVE" -eq 0 ]; then
        printf '\n将进行 %s 次“卸载外置卷 -> 验证不可用 -> 重新挂载 -> 读取并写入”循环。\n' "$EXTERNAL_CYCLES"
        cycle=1
        completed_cycles=0
        # 每轮由用户断开和重连，再读旧文件并添加一项新样本；不能卸载或重连未完成时直接结束剩余循环。
        while [ "$cycle" -le "$EXTERNAL_CYCLES" ]; do
            printf '\n[外置卷循环 %s/%s] 请先卸载 %s，然后按回车。\n' \
                "$cycle" "$EXTERNAL_CYCLES" "$EXTERNAL_DIR"
            printf '如果系统提示卷仍被终端、Finder 或应用占用，请回到对应窗口执行 cd ~，再重试。\n'
            IFS= read -r _ || true

            if [ -e "$EXT/external.veil-link" ]; then
                skip_case "EXTC-03-$cycle" "外置卷仍未卸载，跳过本轮剩余检查"
                break
            fi

            run_case "EXTC-03-disconnect-$cycle" 1 "第 $cycle 次断连后通过本地链接访问应失败" \
                env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" \
                list "$RUN_ROOT/external-local-pointer.veil-link"
            assert_file_missing "EXTC-03-shadow-$cycle" \
                "断连后不得在 /Volumes 下创建影子测试目录" "$EXT"

            printf '\n[外置卷循环 %s/%s] 请重新连接 %s，待卷挂载完成后按回车。\n' \
                "$cycle" "$EXTERNAL_CYCLES" "$EXTERNAL_DIR"
            IFS= read -r _ || true
            if [ ! -e "$EXT/external.veil-link" ]; then
                skip_case "EXTC-04-$cycle" "外置卷尚未重新挂载，停止后续循环"
                break
            fi

            run_case "EXTC-04-reconnect-$cycle" 0 "第 $cycle 次重连后读取容器" \
                env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" \
                list "$RUN_ROOT/external-local-pointer.veil-link"
            run_case "EXTC-04-export-$cycle" 0 "第 $cycle 次重连后导出文件" \
                env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" \
                ex "$RUN_ROOT/external-local-pointer.veil-link" hello.txt \
                "$RUN_ROOT/external-cycle-$cycle-export.txt"
            assert_file_equals "EXTC-04-export-bytes-$cycle" "第 $cycle 次重连后导出内容一致" \
                "$RUN_ROOT/external-cycle-$cycle-export.txt" "$FIXTURES/hello.txt"

            CYCLE_SOURCE="$RUN_ROOT/external-cycle-$cycle-source.txt"
            printf '外置卷循环 %s\n' "$cycle" > "$CYCLE_SOURCE"
            run_case "EXTC-05-add-$cycle" 0 "第 $cycle 次重连后继续添加文件" \
                env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" \
                add "$RUN_ROOT/external-local-pointer.veil-link" "$CYCLE_SOURCE" \
                "cycles/cycle-$cycle.txt"

            # 走完步骤即记为完成轮数，即使 run_case 已登记失败也会增加；末尾数量断言可能进一步暴露写入缺失。
            completed_cycles=$cycle
            cycle=$((cycle + 1))
        done

        if [ "$completed_cycles" -gt 0 ]; then
            EXPECTED_EXTERNAL_ENC=$((EXTERNAL_ENC_BEFORE + completed_cycles))
            assert_values_equal "EXTC-06-count" "多轮重连后新增密文数量正确" \
                "$EXPECTED_EXTERNAL_ENC" "$(count_enc_files "$EXTERNAL_WS")"
            assert_values_equal "EXTC-07-link-hash" "多轮断连重连后链接哈希保持不变" \
                "$EXTERNAL_LINK_HASH_BEFORE" "$(hash_file "$RUN_ROOT/external-local-pointer.veil-link")"
            # 只验证当前元数据能算出非空摘要，没有保存并比较断连前摘要，不表示元数据内容未变化。
            if ACTUAL_EXTERNAL_B3=$(blake3_hex "$EXTERNAL_WS/.veil-info" 2>/dev/null); then
                assert_true "EXTC-07-b3sum" "重连后元数据 BLAKE3 可重新计算" \
                    test -n "$ACTUAL_EXTERNAL_B3"
            else
                skip_case "EXTC-07-b3sum" "无法重算元数据 BLAKE3"
            fi
        fi
    else
        skip_case "EXTC" "非交互模式无法执行真实卸载和重挂载"
    fi

    # 实际在本函数末尾立即尝试删除，非统一 cleanup；KEEP_DATA 开启则保留，异常中断前没有专门外置目录清理 trap。
    printf '\n[观察] 外置测试目录会在最终清理阶段删除。\n'
    if [ "$KEEP_DATA" -eq 0 ]; then
        rm -rf "$EXTERNAL_WS" "$EXT"
    fi
}

# 功能：观察同容器/独立容器并发写入、100 个小文件导入、8 MiB 文件往返和改密并发读取。
# 入参：无；RUN_STRESS 控制启用，依赖 DEBUG_BIN 和 FIXTURES，切换 stress 阶段。
# 返回：更新场景/断言计数，禁用则登记跳过；末尾输出状态通常为 0。
# 异常：并发双方状态只打印，不决定 run_pair 成败；注意：没有进度采样、取消或恢复步骤，也不是吞吐量/资源上限基准测试。
run_stress_section() {
    if [ "$RUN_STRESS" -ne 1 ]; then
        section "CON：并发和较大数据"
        skip_case "CON" "压力测试未启用"
        return
    fi

    section "CON：并发、较大数据和清理"
    phase "stress"

    run_case "CON-init-a" 0 "并发测试容器 A 初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init concurrent
    run_case "CON-init-b" 0 "独立容器 B 初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init independent

    printf 'one\n' > "$RUN_ROOT/concurrent-one.txt"
    printf 'two\n' > "$RUN_ROOT/concurrent-two.txt"
    # 两个独立进程共享容器并发写不同目标；随后只验证 list 能运行，没有解析两个文件是否均存在。
    run_pair "CON-01-parallel-add" "并发向同一容器添加文件" \
        "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add concurrent '$RUN_ROOT/concurrent-one.txt' one.txt" \
        "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add concurrent '$RUN_ROOT/concurrent-two.txt' two.txt"
    run_case "CON-01-observe" 0 "并发添加后查看列表" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list concurrent

    run_pair "CON-09-independent" "并发操作不同容器" \
        "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add concurrent '$FIXTURES/hello.txt' independent-add.txt" \
        "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add independent '$FIXTURES/hello.txt' independent.txt"
    run_case "CON-09-a" 0 "容器 A 仍可读取" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list concurrent
    run_case "CON-09-b" 0 "容器 B 仍可读取" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list independent

    if [ -x "$DEBUG_BIN" ]; then
        # 内嵌循环每轮一个小文件，构造 100 项后整目录导入；数量未独立解析断言，且复用已有 many 目录不会先清空残留。
        run_case "CON-10-100-files" 0 "添加包含 100 个小文件的目录" \
            sh -c "mkdir -p '$RUN_ROOT/many'; i=1; while [ \$i -le 100 ]; do printf 'file-%s\\n' \"\$i\" > '$RUN_ROOT/many/f'\"\$i\".txt; i=\$((i+1)); done; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add concurrent '$RUN_ROOT/many' many"
        run_case "CON-10-verify" 0 "添加 100 个文件后列表验证" \
            env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list concurrent
    fi

    # 8 MiB 全零样本用于字节往返比较；dd 失败后仍会执行 add，最终退出码只来自 add。
    run_case "CON-11-large-add" 0 "添加 8 MiB 较大文件" \
        sh -c "dd if=/dev/zero of='$RUN_ROOT/large.bin' bs=1048576 count=8 >/dev/null 2>&1; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add concurrent '$RUN_ROOT/large.bin' large.bin"
    run_case "CON-11-large-export" 0 "导出 8 MiB 较大文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex concurrent large.bin "$RUN_ROOT/large-export.bin"
    assert_file_equals "CON-11-large-bytes" "large export matches" "$RUN_ROOT/large-export.bin" "$RUN_ROOT/large.bin"

    run_pair "CON-04-passwd-read" "改密与读取并发" \
        "VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=2 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' passwd concurrent" \
        "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list concurrent"
    printf '[观察] 并发改密后密码可能是 1 或 2；后续没有用例依赖该状态。\n'
}

# 功能：对本脚本创建的已知口令测试包，通过固定菜单输入观察字典、枚举、组词和历史功能。
# 入参：无；RUN_BRUTE 控制启用，依赖 DEBUG_BIN、BRUTE_BIN 及 prepare_fixtures 的三份字典，切换 brute 阶段。
# 返回：run_case 累计退出码结果，夹具缺失额外增加 ASSERT_FAIL_COUNT 并跳过后续，末尾历史展示状态通常为 0。
# 异常：不设执行时限，菜单输入不匹配当前工具版本时可能停等；注意：用例大多期望工具正常退出 0，不解析正文证明实际命中或未命中，不能据此评估生产口令强度。
run_brute_force_section() {
    if [ "$RUN_BRUTE" -ne 1 ]; then
        section "BF：密码强度测试工具"
        skip_case "BF" "破解工具场景未启用"
        return
    fi

    section "BF：密码强度测试工具"
    phase "brute"
    RUN_BRUTE=1

    # 测试包仅包含公开样本，已知口令为 1，并启用快速测试 KDF；不读取用户真实容器。
    BRUTE_PACKAGE="$RUN_ROOT/brute-package.veil"
    run_case "BF-fixture-init" 0 "创建密码强度测试 Veil" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init brute-fixture
    run_case "BF-fixture-add" 0 "添加密码强度测试内容" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add brute-fixture "$FIXTURES/hello.txt"
    run_case "BF-fixture-pack" 0 "使用 CLI 打包密码强度测试夹具" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" pack brute-fixture --output "$BRUTE_PACKAGE"
    if [ ! -s "$BRUTE_PACKAGE" ]; then
        ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
        printf '[断言失败] BF 夹具无法创建打包文件\n'
        skip_case "BF" "夹具创建失败，跳过交互式攻击场景"
        return
    fi
    assert_file_exists "BF-fixture" "CLI 打包文件已创建" "$BRUTE_PACKAGE"

    run_case "BF-01-no-path" 0 "未提供路径时进入交互模式并由 q 退出" \
        sh -c "printf 'q\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN'"
    run_case "BF-02-missing-path" 0 "路径不存在后可重新输入" \
        sh -c "printf 'missing.veil\\nq\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN'"

    PACK_FIXTURE="$RUN_ROOT/corrupt-brute-package.veil"
    printf 'not a veil\n' > "$PACK_FIXTURE"
    run_case "BF-02-corrupt-header" 0 "损坏头部后可重新输入" \
        sh -c "printf '$PACK_FIXTURE\\nq\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN'"

    # 三份字典分别包含口令 1、不含口令 1、没有候选；固定菜单行依次选择字典模式、路径和退出。
    run_case "BF-04-dictionary-hit" 0 "字典攻击命中密码 1" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-hit.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-05-dictionary-miss" 0 "字典攻击未命中并给出结果" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-miss.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-06-dictionary-empty" 0 "空字典处理" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-empty.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"

    # 下面输入按当前菜单编号和问答顺序排列，每行是一个回答，不是直接调用底层搜索函数。
    run_case "BF-08-custom-charset" 0 "自定义数字字符集命中密码 1" \
        sh -c "printf '2\\n3\\n1\\n1\\ny\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-09-invalid-range" 0 "非法长度范围被修正并处理" \
        sh -c "printf '2\\n1\\n9\\n2\\nn\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-11-preset" 0 "预设数字攻击命中密码 1" \
        sh -c "printf '3\\n1\\n1\\n1\\ny\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-12-word-combination" 0 "组词攻击命中密码 1" \
        sh -c "printf '4\\n1 2\\n1\\n1\\ny\\ny\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-14-thread-zero" 0 "线程数 0 被修正后处理" \
        sh -c "printf '5\\n0\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-16-switch-veil" 0 "切换容器菜单流程" \
        sh -c "printf '6\\n$BRUTE_PACKAGE\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-18-history" 0 "历史记录写入并显示" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-miss.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"
    run_case "BF-20-retry-cancel" 0 "可取消重试历史密码" \
        sh -c "printf '7\\nn\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$BRUTE_PACKAGE'"

    printf '\n[BF 历史记录]\n'
    find "$PHASE_HOME/.veil_history" -type f -maxdepth 1 -print -exec sed -n '1,80p' {} \; 2>/dev/null || true
}

# 功能：打印汇总计数、二进制摘要、疑似 panic 文本和部分残留文件。
# 入参：无；读取计数、二进制及 RUN_ROOT/cases 报告；返回最后一条输出管道的状态。
# 异常：读取错误多被忽略；注意：发现 panic 标记只作为观察，不增加 FAIL_COUNT，残留文件仅展示前 200 行，也不据此判失败。
run_final_observations() {
    section "最终观察与清理"
    printf 'Debug 二进制: %s\n' "$DEBUG_BIN"
    printf 'Release 二进制: %s\n' "$RELEASE_BIN"
    printf 'Debug CLI SHA-256: %s\n' "$(hash_file "$DEBUG_BIN" 2>/dev/null || printf 不可用)"
    printf 'Release CLI SHA-256: %s\n' "$(hash_file "$RELEASE_BIN" 2>/dev/null || printf 不可用)"
    printf '\n用例统计:\n'
    printf '  通过: %s\n' "$PASS_COUNT"
    printf '  失败: %s\n' "$FAIL_COUNT"
    printf '  跳过: %s\n' "$SKIP_COUNT"
    printf '  断言失败: %s\n' "$ASSERT_FAIL_COUNT"

    printf '\n潜在 panic 标记:\n'
    if grep -R -E -n 'panicked at|thread .* panicked|RUST_BACKTRACE|fatal runtime error' "$RUN_ROOT/cases" 2>/dev/null; then
        printf '[观察] 上方发现 panic 标记\n'
    else
        printf '捕获的命令输出中未发现\n'
    fi

    printf '\n清理前测试根目录中的剩余临时文件:\n'
    find "$RUN_ROOT" -name '*.tmp' -o -name '*.new' -o -name '*.bak' | LC_ALL=C sort | sed -n '1,200p'
}

# 功能：按依赖顺序编排整轮黑盒测试，并用自动场景和断言失败计数决定最终状态。
# 入参：全部脚本参数转交 parse_args；返回：有 FAIL_COUNT 或 ASSERT_FAIL_COUNT 时为 1，否则 0，前置辅助程序失败也返回 1。
# 异常：部分前置检查直接 exit；信号处理映射为 INT=130、HUP=129、TERM=143，EXIT 执行 cleanup。
# 注意：场景复用全局状态并顺序运行，不能随意独立或并发调用；通过计数也包含观察类用例，最终 0 不代表所有可选项都执行或所有输出都无错误。
main() {
    parse_args "$@"

    start_logging
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 129' HUP
    trap 'exit 143' TERM

    section "Veil 全功能场景测试"
    printf '开始时间: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
    printf '测试根目录: %s\n' "$RUN_ROOT"
    printf '报告根目录: %s\n' "$REPORT_ROOT"
    printf '自动密码: 1；交互提示需要手动输入\n'
    printf 'Debug KDF 覆盖: VEIL_TEST_KDF=fast\n'

    mkdir -p "$RUN_ROOT" "$REPORT_ROOT" "$RUN_ROOT/cases"
    prepare_fixtures
    prompt_optional_areas
    ensure_binaries

    printf '// JSONC 解析器检查\n{"ready": true,}\n' > "$RUN_ROOT/jsonc-helper-check.jsonc"
    if ! jsonc_tool get "$RUN_ROOT/jsonc-helper-check.jsonc" /ready >/dev/null; then
        printf '无法构建或运行 JSONC 测试辅助程序。\n' >&2
        return 1
    fi

    # 从入口参数到初始化、导入、持久化、修改和打包逐步建立后续夹具，后续场景依赖先前写入的全局路径。
    run_global_help_section "debug" "$DEBUG_BIN"
    run_init_section
    run_add_view_section
    run_persistence_artifact_section
    run_mutation_operations_section
    run_pack_unpack_section
    run_config_link_section
    run_shell_section
    run_corruption_section
    run_e2e_section
    # 可选场景内部根据标记判断跳过；非交互模式不必然禁用命令行已显式开启的测试。
    run_external_section
    run_stress_section
    run_brute_force_section
    if [ "$QUICK" -eq 0 ]; then
        run_release_section
    else
        section "Release 构建"
        skip_case "REL" "快速模式跳过 Release 覆盖"
    fi
    run_final_observations

    printf '\n完成时间: %s\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"

    if [ "$FAIL_COUNT" -gt 0 ] || [ "$ASSERT_FAIL_COUNT" -gt 0 ]; then
        return 1
    fi
    return 0
}

main "$@"
