#!/bin/sh
#
# Veil 全功能黑盒场景测试脚本。
#
# 脚本保持自包含，所有运行数据都放在带时间戳的临时目录中，不读取或修改
# 调用者真实的 ~/.veil 目录。
#
# 常用方式：
#   sh scripts/full-scenario-test.sh
#   sh scripts/full-scenario-test.sh --quick
#   sh scripts/full-scenario-test.sh --external /Volumes/MyDisk
#   sh scripts/full-scenario-test.sh --non-interactive --no-build
#
# 自动测试统一使用密码 "1"；交互测试会要求用户直接在终端输入密码。

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
PROJECT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)

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

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
NOTE_COUNT=0
CHECK_COUNT=0
ASSERT_FAIL_COUNT=0

CURRENT_PHASE=setup
PHASE_HOME="$RUN_ROOT/home"
PHASE_WORK="$RUN_ROOT/work"
CASE_OUT="$RUN_ROOT/.case.out"
CASE_ERR="$RUN_ROOT/.case.err"
BLAKE3_HELPER_BIN=""

log_line() {
    printf '%s\n' "$*"
}

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

section() {
    printf '\n'
    printf '%s\n' "============================================================"
    printf '%s\n' "$*"
    printf '%s\n' "============================================================"
}

note() {
    NOTE_COUNT=$((NOTE_COUNT + 1))
    printf '[说明 %03d] %s\n' "$NOTE_COUNT" "$*"
}

skip_case() {
    SKIP_COUNT=$((SKIP_COUNT + 1))
    printf '[跳过] %-10s %s\n' "$1" "$2"
}

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

hash_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        cksum "$1" | awk '{print $1 ":" $2}'
    fi
}

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
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        std::process::exit(2);
    }
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

file_size() {
    wc -c < "$1" | tr -d ' '
}

file_mode() {
    stat -f '%Lp' "$1" 2>/dev/null || stat -c '%a' "$1" 2>/dev/null
}

binary_is_stale() {
    binary=$1
    [ -x "$binary" ] || return 0
    find "$PROJECT_ROOT/crates" "$PROJECT_ROOT/Cargo.toml" "$PROJECT_ROOT/Cargo.lock" \
        -type f \( -name '*.rs' -o -name 'Cargo.toml' \) \
        -newer "$binary" -print -quit 2>/dev/null | grep -q .
}

count_enc_files() {
    find "$1" -maxdepth 1 -type f -name '*.enc' ! -name '._*' | wc -l | tr -d ' '
}

workspace_file_bytes() {
    find "$1" -type f -print | while IFS= read -r file; do
        file_size "$file"
    done | awk '{ total += $1 } END { print total + 0 }'
}

package_entries_match_workspace() {
    package=$1
    workspace=$2
    expected_count=$3
    metadata_size=$(dd if="$package" bs=1 skip=14 count=4 2>/dev/null | od -An -tu4 | tr -d ' \n')
    position=$((22 + metadata_size))
    index=0

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
        for encrypted in "$workspace"/*.enc; do
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

snapshot_workspace() {
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

capture_snapshot() {
    label=$1
    path=$2
    out="$RUN_ROOT/snapshot-$CURRENT_PHASE-$label.txt"
    snapshot_workspace "$path" > "$out"
    printf '[快照] %s\n' "$out"
    cat "$out"
}

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

assert_file_exists() {
    assert_true "$1" "$2" test -e "$3"
}

assert_file_missing() {
    assert_true "$1" "$2" test ! -e "$3"
}

assert_file_equals() {
    assert_true "$1" "$2" cmp -s "$3" "$4"
}

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

run_shell_case() {
    id=$1
    expected=$2
    description=$3
    command_text=$4
    run_case "$id" "$expected" "$description" /bin/sh -c "$command_text"
}

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
    printf '[观察] %s 已完成，测试脚本未死锁\n' "$id"
}

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

parse_args() {
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

finish_logging() {
    if [ -n "${LOG_TEE_PID:-}" ]; then
        exec 1>&3 2>&4
        exec 3>&-
        exec 4>&-
        wait "$LOG_TEE_PID" 2>/dev/null || true
        rm -f "$LOG_FIFO"
    fi
}

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
    run_case "GLB-27-source" 2 "缺少添加源路径" env VEIL_HINTS=off "$BIN" add container-only
    run_case "GLB-27-output" 2 "缺少导出输出路径" env VEIL_HINTS=off "$BIN" ex container-only input.txt
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

    LONG_PASSWORD=$(awk 'BEGIN { for (i = 0; i < 1024; i++) printf "x" }')
    run_case "PWD-06-long" 0 "使用 1024 字节密码初始化" \
        env VEIL_PASSWORD="$LONG_PASSWORD" VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init long-pass
    run_case "PWD-06-long-open" 0 "使用 1024 字节密码重新打开" \
        env VEIL_PASSWORD="$LONG_PASSWORD" VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list long-pass

    run_case "INIT-01-default" 0 "默认容器初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=full VEIL_TEST_KDF=fast "$DEBUG_BIN" init core
    assert_file_exists "INIT-01-link" "core 链接存在" "$PHASE_WORK/core.veil-link"
    assert_file_exists "INIT-01-meta" "默认工作区包含元数据" \
        "$PHASE_HOME/.veil/workspaces/default"
    assert_file_exists "INIT-01-config" "全局配置已创建" "$PHASE_HOME/.veil/config.toml"

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
        --workspace default
    run_case "INIT-14-missing-named" 1 "拒绝不存在的命名工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init missing-workspace \
        --workspace does-not-exist

    CUSTOM_ROOT="$RUN_ROOT/custom-workspace"
    mkdir -p "$CUSTOM_ROOT"
    cat >> "$PHASE_HOME/.veil/config.toml" <<EOF

[workspace.custom.lab]
path = "$CUSTOM_ROOT"
workspace_type = { Custom = "lab" }
created_at = "2026-01-01T00:00:00+00:00"
EOF
    run_case "INIT-14-named" 0 "使用已注册的命名工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init named-custom \
        --workspace lab

    DEDICATED="$RUN_ROOT/dedicated-empty"
    mkdir -p "$DEDICATED"
    run_case "INIT-19-dedicated-empty" 0 "使用空目录作为专属工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated \
        --workspace-path "$DEDICATED" --dedicated --link "$PHASE_WORK/dedicated.veil-link"
    assert_file_exists "INIT-19-meta" "专属目录根下直接存在元数据" "$DEDICATED/.veil-meta"

    NONEMPTY="$RUN_ROOT/dedicated-nonempty"
    mkdir -p "$NONEMPTY"
    printf 'keep\n' > "$NONEMPTY/keep.txt"
    printf 'keep\n' > "$RUN_ROOT/dedicated-keep-expected.txt"
    run_case "INIT-20-dedicated-nonempty" 1 "拒绝非空专属工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-nonempty \
        --workspace-path "$NONEMPTY" --dedicated --link "$PHASE_WORK/dedicated-nonempty.veil-link"
    assert_file_equals "INIT-20-preserved" "原有专属目录文件保持不变" \
        "$NONEMPTY/keep.txt" "$RUN_ROOT/dedicated-keep-expected.txt"

    DEDICATED_NEW="$RUN_ROOT/dedicated-new"
    run_case "INIT-21-dedicated-create" 0 "创建不存在的专属工作区目录" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-new \
        --workspace-path "$DEDICATED_NEW" --dedicated --link "$PHASE_WORK/dedicated-new.veil-link"

    DEDICATED_FILE="$RUN_ROOT/dedicated-file"
    printf 'not a directory\n' > "$DEDICATED_FILE"
    run_case "INIT-22-dedicated-file" 1 "拒绝将普通文件作为专属工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-file \
        --workspace-path "$DEDICATED_FILE" --dedicated --link "$PHASE_WORK/dedicated-file.veil-link"

    run_case "INIT-23-conflict" 1 "命名工作区和显式路径冲突" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init conflict-workspace \
        --workspace default --workspace-path "$RUN_ROOT/conflict"
    run_case "INIT-24-dedicated-no-path" 1 "专属模式缺少路径时拒绝" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init dedicated-no-path \
        --dedicated
    run_case "INIT-25-portable-conflict" 1 "便携模式与工作区选项冲突" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init portable-conflict \
        --portable --workspace-path "$RUN_ROOT/portable-conflict"

    PORTABLE="$RUN_ROOT/portable"
    mkdir -p "$PORTABLE"
    run_case "INIT-26-portable" 0 "显式便携布局" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init portable \
        --portable --link "$PORTABLE/portable.veil-link"
    assert_file_exists "INIT-26-workspace" "便携工作区位于链接目录下" \
        "$PORTABLE/.veil/workspaces/default"

    REL_ROOT="$RUN_ROOT/relative-workspace-parent"
    mkdir -p "$REL_ROOT"
    (
        cd "$REL_ROOT" || exit 1
        VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init relative \
            --workspace-path relative-workspace --link "$REL_ROOT/relative.veil-link"
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

    BLOCKER="$RUN_ROOT/blocker"
    printf 'blocker\n' > "$BLOCKER"
    run_case "INIT-29-rollback" 1 "链接注册失败时回滚工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init rollback \
        --link "$BLOCKER/nested.veil-link"

    printf '\n[权限] 初始化产物\n'
    stat -f '%Sp %N' "$PHASE_WORK/core.veil-link" "$PHASE_HOME/.veil/config.toml" 2>/dev/null || \
        stat -c '%A %n' "$PHASE_WORK/core.veil-link" "$PHASE_HOME/.veil/config.toml" 2>/dev/null || true
}

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

    printf 'replacement-first\n' > "$RUN_ROOT/replace.txt"
    run_case "ADD-22-first" 0 "替换前先添加文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$RUN_ROOT/replace.txt"
    before_enc_count=$(find "$(resolve_workspace_for_link "$PHASE_WORK/fileops.veil-link")" -name '*.enc' | wc -l | tr -d ' ')
    printf 'replacement-second\n' > "$RUN_ROOT/replace.txt"
    run_case "ADD-22-replace" 0 "替换已有目标" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$RUN_ROOT/replace.txt"

    run_case "ADD-26-wrong-password" 1 "错误密码添加失败" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add fileops "$FIXTURES/hello.txt" wrong-add.txt

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
    run_case "EX-04-missing-parent" 1 "输出父目录不存在时处理一致" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops hello.txt "$RUN_ROOT/no-such-parent/x.bin"
    run_case "EX-07-missing" 1 "导出不存在的容器内文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex fileops missing.bin "$RUN_ROOT/export/missing.bin"
    assert_file_missing "EX-07-no-file" "导出不存在文件时不应创建输出" "$RUN_ROOT/export/missing.bin"
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

resolve_workspace_for_link() {
    link=$1
    if [ ! -f "$link" ]; then
        return 1
    fi
    rel=$(awk -F'"' '/^path = / {print $2; exit}' "$link")
    printf '/%s\n' "$rel"
}

run_persistence_artifact_section() {
    section "CONF/ART：三个核心配置文件和运行产物内容检查"
    phase "persistence"

    ARTIFACT_SECRET="VEIL_PLAINTEXT_MARKER_20260912"
    FIRST_SOURCE="$RUN_ROOT/artifact-first.txt"
    SECOND_SOURCE="$RUN_ROOT/artifact-second.txt"
    REPLACEMENT_SOURCE="$RUN_ROOT/artifact-first-replaced.txt"
    printf '%s\n' "$ARTIFACT_SECRET first version" > "$FIRST_SOURCE"
    printf '%s\n' "$ARTIFACT_SECRET second file" > "$SECOND_SOURCE"
    printf '%s\n' "$ARTIFACT_SECRET replacement version" > "$REPLACEMENT_SOURCE"

    run_case "CONF-00" 0 "初始化三个核心配置文件测试容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init artifact-check

    ARTIFACT_LINK="$PHASE_WORK/artifact-check.veil-link"
    ARTIFACT_CONFIG="$PHASE_HOME/.veil/config.toml"
    ARTIFACT_WS=$(resolve_workspace_for_link "$ARTIFACT_LINK")
    ARTIFACT_META="$ARTIFACT_WS/.veil-meta"
    LINK_HASH_BEFORE=$(hash_file "$ARTIFACT_LINK")

    assert_file_exists "CONF-01" "全局配置存在" "$ARTIFACT_CONFIG"
    assert_file_exists "CONF-01-link" "链接文件存在" "$ARTIFACT_LINK"
    assert_file_exists "CONF-01-meta" "元数据文件存在" "$ARTIFACT_META"
    assert_contains "CONF-01-version" "配置包含版本" "version =" "$ARTIFACT_CONFIG"
    assert_contains "CONF-02-container" "配置包含容器记录" "[containers." "$ARTIFACT_CONFIG"
    assert_contains "CONF-02-id" "容器记录包含稳定 ID" "veil_id =" "$ARTIFACT_CONFIG"
    assert_contains "CONF-03-volume" "配置包含卷记录" "[volumes." "$ARTIFACT_CONFIG"
    assert_contains "CONF-04-link-record" "配置包含链接缓存记录" "raw_hex =" "$ARTIFACT_CONFIG"
    assert_contains "CONF-04-hash" "配置包含链接内容哈希" "content_hash =" "$ARTIFACT_CONFIG"

    LINK_ID=$(awk -F'"' '/^veil_id = / { print $2; exit }' "$ARTIFACT_LINK")
    LINK_NAME=$(awk -F'"' '/^container_name = / { print $2; exit }' "$ARTIFACT_LINK")
    LINK_PATH=$(awk -F'"' '/^path = / { print $2; exit }' "$ARTIFACT_LINK")
    META_STRINGS="$RUN_ROOT/cases/CONF-09-meta-strings.txt"
    strings "$ARTIFACT_META" > "$META_STRINGS"
    META_ID=$(grep -Eo 'veil-[0-9a-f]+' "$META_STRINGS" | head -n 1)
    assert_values_equal "CONF-09-id" "链接与元数据稳定 ID 一致" "$LINK_ID" "$META_ID"
    assert_contains "CONF-09-name" "元数据明文头包含容器名称" "$LINK_NAME" "$META_STRINGS"
    assert_contains "CONF-09-type" "元数据明文头包含工作区类型" "default" "$META_STRINGS"
    assert_contains "CONF-07-version" "链接包含版本号" "version = \"1.0\"" "$ARTIFACT_LINK"
    assert_contains "CONF-07-algorithm" "链接包含加密算法" "ChaCha20-Poly1305" "$ARTIFACT_LINK"
    assert_contains "CONF-07-kdf" "链接包含 KDF" "Argon2id" "$ARTIFACT_LINK"
    assert_not_contains "CONF-08-secret" "链接不包含明文密码或内容标记" "$ARTIFACT_SECRET" "$ARTIFACT_LINK"

    case "$LINK_PATH" in
        /*)
            printf '[断言失败] %-10s 链接保存了绝对工作区路径（%s）\n' "CONF-08-relative" "$LINK_PATH"
            CHECK_COUNT=$((CHECK_COUNT + 1))
            ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
            ;;
        *)
            printf '[断言通过] %-10s 链接使用相对卷路径（%s）\n' "CONF-08-relative" "$LINK_PATH"
            CHECK_COUNT=$((CHECK_COUNT + 1))
            ;;
    esac

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
    assert_contains "CONF-09-version" "元数据 TLV 版本号可解析" "version=1" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_contains "CONF-09-algorithm" "元数据 TLV 算法标识正确" "algorithm=2" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_contains "CONF-09-salt" "元数据 TLV 盐长度为 32" "salt_len=32" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_contains "CONF-09-nonce" "元数据 TLV nonce 长度为 12" "nonce_len=12" \
        "$RUN_ROOT/cases/CONF-09-tlv-fields.txt"
    assert_not_contains "CONF-10-meta-secret" "元数据不包含明文内容标记" "$ARTIFACT_SECRET" "$ARTIFACT_META"

    RAW_HEX=$(awk -F'"' '/^raw_hex = / { print $2; exit }' "$ARTIFACT_CONFIG")
    if [ -n "$RAW_HEX" ] && command -v xxd >/dev/null 2>&1; then
        printf '%s' "$RAW_HEX" | xxd -r -p > "$RUN_ROOT/decoded-link.bin"
        assert_file_equals "CONF-05-raw-hex" "配置 raw_hex 解码后与原链接逐字节一致" \
            "$RUN_ROOT/decoded-link.bin" "$ARTIFACT_LINK"
    else
        printf '[断言失败] %-10s 无法验证 raw_hex：缓存为空或缺少 xxd\n' "CONF-05-raw-hex"
        CHECK_COUNT=$((CHECK_COUNT + 1))
        ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    fi

    CONFIG_HASH=$(awk -F'"' '/^content_hash = / { print $2; exit }' "$ARTIFACT_CONFIG")
    ACTUAL_B3=$(blake3_hex "$ARTIFACT_LINK" 2>/dev/null || true)
    if [ -n "$ACTUAL_B3" ]; then
        assert_values_equal "CONF-06-blake3" "配置 content_hash 与链接 BLAKE3 一致" "$CONFIG_HASH" "$ACTUAL_B3"
    else
        skip_case "CONF-06" "无法构建或找到 BLAKE3 重算工具"
    fi

    assert_private_mode "CONF-15-config-mode" "全局配置仅当前用户可读写" "$ARTIFACT_CONFIG"
    assert_private_mode "CONF-15-link-mode" "链接文件仅当前用户可读写" "$ARTIFACT_LINK"
    assert_private_mode "CONF-15-meta-mode" "元数据仅当前用户可读写" "$ARTIFACT_META"

    run_case "ART-01-add" 0 "添加第一个可识别明文文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$FIRST_SOURCE" first.txt
    ENC_COUNT=$(count_enc_files "$ARTIFACT_WS")
    assert_values_equal "ART-01-count" "单文件添加后密文数量增加一" "1" "$ENC_COUNT"
    FIRST_SIZE=$(file_size "$FIRST_SOURCE")
    FIRST_ENC=$(find "$ARTIFACT_WS" -maxdepth 1 -type f -name '*.enc' | head -n 1)
    ENC_SIZE=$(file_size "$FIRST_ENC")
    assert_values_equal "ART-01-tag" "密文大小等于明文加 16 字节认证标签" "$((FIRST_SIZE + 16))" "$ENC_SIZE"
    assert_not_contains "ART-03-enc-secret" "密文不含明文内容标记" "$ARTIFACT_SECRET" "$FIRST_ENC"

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
    EMPTY_ENC_SIZE=$(for encrypted in "$ARTIFACT_WS"/*.enc; do file_size "$encrypted"; done | sort -n | head -n 1)
    assert_values_equal "ART-02-tag" "空文件密文大小等于 16 字节认证标签" "16" "$EMPTY_ENC_SIZE"

    run_case "ART-06-add-second" 0 "添加第二个文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add artifact-check \
        "$SECOND_SOURCE" second.txt
    assert_values_equal "ART-06-second-count" "第二个文件添加后密文数量正确" "3" "$(count_enc_files "$ARTIFACT_WS")"

    MANY_DIR="$RUN_ROOT/artifact-many"
    mkdir -p "$MANY_DIR"
    index=1
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

    ACTUAL_BYTES=$(workspace_file_bytes "$ARTIFACT_WS")
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
        package_entries_match_workspace "$PACKAGE" "$ARTIFACT_WS" "$PACKAGE_FILE_COUNT"
    assert_not_contains "ART-04-package-secret" "包内不含明文内容标记" "$ARTIFACT_SECRET" "$PACKAGE"

    CURRENT_LINK_HASH=$(hash_file "$ARTIFACT_LINK")
    assert_values_equal "CONF-11-link-hash" "文件操作不改变链接哈希" "$LINK_HASH_BEFORE" "$CURRENT_LINK_HASH"

    META_HASH_BEFORE_ROTATE=$(hash_file "$ARTIFACT_META")
    CONFIG_ID_BEFORE_ROTATE=$(awk -F'"' '/^veil_id = / { print $2; exit }' "$ARTIFACT_CONFIG")
    run_case "CONF-12-rotate" 0 "修改密码以检查三个文件变化" \
        env VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=2 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd artifact-check
    assert_values_not_equal "CONF-12-meta-change" "改密后元数据字节发生变化" \
        "$META_HASH_BEFORE_ROTATE" "$(hash_file "$ARTIFACT_META")"
    assert_values_equal "CONF-12-link-stable" "改密后链接字节保持不变" \
        "$CURRENT_LINK_HASH" "$(hash_file "$ARTIFACT_LINK")"
    CONFIG_ID_AFTER_ROTATE=$(awk -F'"' '/^veil_id = / { print $2; exit }' "$ARTIFACT_CONFIG")
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
    find "$ARTIFACT_WS" -maxdepth 1 -type f -print | LC_ALL=C sort | while IFS= read -r file; do
        describe_file "$file"
    done
}

run_mutation_operations_section() {
    section "RM/MV/PASSWD：变更、冲突和密码轮换"
    phase "mutate"

    run_case "MUT-init" 0 "变更测试容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init mutate
    run_case "MUT-add-tree" 0 "准备目录树数据" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add mutate "$FIXTURES/tree"
    run_case "MUT-add-file" 0 "准备普通文件数据" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add mutate "$FIXTURES/hello.txt" keep.txt

    WS=$(resolve_workspace_for_link "$PHASE_WORK/mutate.veil-link")
    capture_snapshot "before-mutations" "$WS"

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
    if grep -Fq 'secret-rotate-value' "$RUN_ROOT/cases/PWD-16-no-leak.stdout" "$RUN_ROOT/cases/PWD-16-no-leak.stderr" "$PHASE_HOME/.veil/config.toml"; then
        printf '[断言失败] PWD-16 密码泄露到可观察输出或配置中\n'
        ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
    else
        printf '[断言通过] PWD-16 输出和配置中未发现密码\n'
    fi
    CHECK_COUNT=$((CHECK_COUNT + 1))
    run_case "PWD-16-rotate-final" 0 "恢复密码 1" \
        env VEIL_PASSWORD=secret-rotate-value VEIL_NEW_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" passwd mutate
}

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
    printf '\n[格式] 包头部和可见路径\n'
    dd if="$PACKAGE" bs=1 count=22 2>/dev/null | od -An -tx1 -v
    printf '魔数: '
    dd if="$PACKAGE" bs=1 count=8 2>/dev/null
    printf '\n包内可见的载荷名称（用于确认当前格式行为）:\n'
    strings "$PACKAGE" | grep -E 'payload\.txt|binary\.bin|文字' || true

    run_case "UNPACK-01" 0 "解包到默认工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name restored
    assert_file_exists "UNPACK-01-link" "解包后的链接存在" "$PHASE_WORK/restored.veil-link"
    run_case "UNPACK-02-export" 0 "解包后导出文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" ex restored payload.txt "$RUN_ROOT/restored-payload.txt"
    assert_file_equals "UNPACK-02-bytes" "解包后的内容一致" \
        "$RUN_ROOT/restored-payload.txt" "$FIXTURES/hello.txt"
    run_case "UNPACK-03-name-extract" 0 "从包文件名推导容器名" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack \
        "$RUN_ROOT/pack-parent/nested/package.veil"
    run_case "UNPACK-04-custom-name" 0 "解包时指定展示名称" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name custom-name --link "$PHASE_WORK/custom-unpack.veil-link"
    run_case "UNPACK-06-missing-workspace" 1 "解包时拒绝未知命名工作区" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name workspace-missing --workspace no-such-workspace
    run_case "UNPACK-08-bad-extension" 1 "解包时检查链接扩展名" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name bad-link --link "$PHASE_WORK/bad-unpack.txt"
    run_case "UNPACK-09-existing-link" 1 "解包拒绝覆盖已有链接" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name existing --link "$PHASE_WORK/restored.veil-link"

    run_case "UNPACK-11-wrong-password" 1 "错误密码在创建产物前失败" \
        env VEIL_PASSWORD=wrong VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$PACKAGE" \
        --name wrong-unpack --link "$PHASE_WORK/wrong-unpack.veil-link"
    assert_file_missing "UNPACK-11-link" "错误密码解包不应留下链接" "$PHASE_WORK/wrong-unpack.veil-link"

    printf '\n[完整性] 畸形包副本\n'
    trunc="$RUN_ROOT/truncated.veil"
    dd if="$PACKAGE" of="$trunc" bs=1 count=10 2>/dev/null
    run_case "UNPACK-12-truncated" 1 "拒绝截断的包" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$trunc" \
        --name trunc --link "$PHASE_WORK/trunc.veil-link"

    bad_magic="$RUN_ROOT/bad-magic.veil"
    cp "$PACKAGE" "$bad_magic"
    printf 'X' | dd of="$bad_magic" bs=1 seek=0 conv=notrunc 2>/dev/null
    run_case "UNPACK-12-magic" 1 "拒绝错误魔数" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_magic" \
        --name magic --link "$PHASE_WORK/magic.veil-link"

    bad_version="$RUN_ROOT/bad-version.veil"
    cp "$PACKAGE" "$bad_version"
    printf '\002\000' | dd of="$bad_version" bs=1 seek=8 conv=notrunc 2>/dev/null
    run_case "UNPACK-13-version" 1 "拒绝不支持的包版本" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_version" \
        --name version --link "$PHASE_WORK/version.veil-link"

    bad_header_size="$RUN_ROOT/bad-header-size.veil"
    cp "$PACKAGE" "$bad_header_size"
    printf '\027\000\000\000' | dd of="$bad_header_size" bs=1 seek=10 conv=notrunc 2>/dev/null
    run_case "UNPACK-13-header-size" 1 "拒绝不支持的包头长度" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_header_size" \
        --name header-size --link "$PHASE_WORK/header-size.veil-link"

    bad_meta_len="$RUN_ROOT/bad-meta-len.veil"
    cp "$PACKAGE" "$bad_meta_len"
    printf '\000\000\000\004' | dd of="$bad_meta_len" bs=1 seek=14 conv=notrunc 2>/dev/null
    run_case "UNPACK-14-meta-len" 1 "拒绝超过文件的元数据长度" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_meta_len" \
        --name meta-len --link "$PHASE_WORK/meta-len.veil-link"

    bad_count_low="$RUN_ROOT/bad-count-low.veil"
    cp "$PACKAGE" "$bad_count_low"
    printf '\000\000\000\000' | dd of="$bad_count_low" bs=1 seek=18 conv=notrunc 2>/dev/null
    run_case "UNPACK-15-count-low" 1 "拒绝少于元数据声明的文件数" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_count_low" \
        --name count-low --link "$PHASE_WORK/count-low.veil-link"

    bad_count_high="$RUN_ROOT/bad-count-high.veil"
    cp "$PACKAGE" "$bad_count_high"
    printf '\377\377\377\377' | dd of="$bad_count_high" bs=1 seek=18 conv=notrunc 2>/dev/null
    run_case "UNPACK-16-count-high" 1 "拒绝多于元数据声明的文件数" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" unpack "$bad_count_high" \
        --name count-high --link "$PHASE_WORK/count-high.veil-link"

    oversized_meta="$RUN_ROOT/oversized-meta.veil"
    cp "$PACKAGE" "$oversized_meta"
    printf '\000\000\000\100' | dd of="$oversized_meta" bs=1 seek=14 conv=notrunc 2>/dev/null
    run_shell_case "UNPACK-22-bounded-allocation" 1 \
        "声明 64 MiB 元数据时应有界失败" \
        "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$oversized_meta' --name oversized --link '$PHASE_WORK/oversized.veil-link'"

    PACK_WS=$(resolve_workspace_for_link "$PHASE_WORK/package.veil-link")
    run_case "PACK-08-missing-ciphertext" 1 "密文缺失时打包失败" \
        sh -c "find '$PACK_WS' -name '*.enc' | head -n 1 | xargs rm -f; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack package --output '$RUN_ROOT/missing-ciphertext.veil'"
    assert_file_missing "PACK-08-output" "密文缺失时不应留下包文件" "$RUN_ROOT/missing-ciphertext.veil"
}

run_config_link_section() {
    section "CFG/LINK/RES：配置、链接恢复和篡改处理"
    phase "config-link"

    run_case "CFG-01" 0 "无配置文件的配置查看" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" config
    assert_file_missing "CFG-01-no-side-effect" "查看配置不应创建配置文件" "$PHASE_HOME/.veil/config.toml"
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
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link linked --output "$PHASE_WORK/second.veil-link"
    assert_file_equals "LINK-01-bytes" "第二个链接与原链接逐字节一致" \
        "$PHASE_WORK/linked.veil-link" "$PHASE_WORK/second.veil-link"

    cp "$PHASE_WORK/second.veil-link" "$RUN_ROOT/link-original.veil-link"
    rm "$PHASE_WORK/second.veil-link"
    run_case "RES-13-restore" 0 "从缓存字节恢复已删除链接" \
        env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" list second.veil-link
    assert_file_equals "RES-13-bytes" "恢复后的链接与原字节一致" \
        "$PHASE_WORK/second.veil-link" "$RUN_ROOT/link-original.veil-link"

    cp "$PHASE_HOME/.veil/config.toml" "$RUN_ROOT/config-before-corruption.toml"
    sed 's/raw_hex = "[0-9a-f][0-9a-f]/raw_hex = "zz/' "$RUN_ROOT/config-before-corruption.toml" > "$RUN_ROOT/config-bad-hex.toml"
    cp "$RUN_ROOT/config-bad-hex.toml" "$PHASE_HOME/.veil/config.toml"
    rm -f "$PHASE_WORK/second.veil-link"
    run_case "RES-15-bad-hex" 1 "拒绝损坏的链接缓存十六进制" \
        env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" list second.veil-link
    cp "$RUN_ROOT/config-before-corruption.toml" "$PHASE_HOME/.veil/config.toml"

    cp "$PHASE_HOME/.veil/config.toml" "$RUN_ROOT/config-before-hash.toml"
    sed 's/^content_hash = ".*"/content_hash = "0000000000000000000000000000000000000000000000000000000000000000"/' \
        "$RUN_ROOT/config-before-hash.toml" > "$RUN_ROOT/config-bad-hash.toml"
    cp "$RUN_ROOT/config-bad-hash.toml" "$PHASE_HOME/.veil/config.toml"
    rm -f "$PHASE_WORK/second.veil-link"
    run_case "RES-14-bad-hash" 1 "拒绝链接缓存哈希不匹配" \
        env VEIL_PASSWORD=1 VEIL_HINTS=brief VEIL_TEST_KDF=fast "$DEBUG_BIN" list second.veil-link
    cp "$RUN_ROOT/config-before-hash.toml" "$PHASE_HOME/.veil/config.toml"
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

    run_case "RES-06-workspace-path" 0 "直接使用工作区路径访问" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$(resolve_workspace_for_link "$PHASE_WORK/linked.veil-link")"
    run_case "RES-07-ordinary-directory" 1 "拒绝普通目录作为容器" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$FIXTURES"
    run_case "RES-08-package-direct" 1 "直接访问包文件时提示先解包" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/custom-package.veil"

    run_case "RES-23-invalid-link" 1 "拒绝格式损坏的链接" \
        sh -c "printf 'not toml\\n' > '$PHASE_WORK/broken.veil-link'; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list '$PHASE_WORK/broken.veil-link'"

    cp "$PHASE_WORK/linked.veil-link" "$RUN_ROOT/link-absolute-source.veil-link"
    ws=$(resolve_workspace_for_link "$PHASE_WORK/linked.veil-link")
    sed "s#^path = .*#path = \"$ws\"#" "$RUN_ROOT/link-absolute-source.veil-link" > "$PHASE_WORK/absolute.veil-link"
    run_case "RES-24-absolute-link-path" 1 "链接中的绝对工作区路径应被拒绝或限制" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$PHASE_WORK/absolute.veil-link"

    cp "$PHASE_HOME/.veil/config.toml" "$RUN_ROOT/config-good.toml"
    printf 'this is not valid = toml = at all\n' > "$PHASE_HOME/.veil/config.toml"
    run_case "CFG-10-corrupt" 1 "拒绝损坏的配置文件" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" config show
    cp "$RUN_ROOT/config-good.toml" "$PHASE_HOME/.veil/config.toml"

    printf '\n[配置] 最终配置:\n'
    sed -n '1,260p' "$PHASE_HOME/.veil/config.toml"

    if [ "$RUN_STRESS" -eq 1 ]; then
        run_pair "CFG-17-parallel" "并发写入提示级别配置" \
            "VEIL_HINTS= VEIL_TEST_KDF=fast '$DEBUG_BIN' config --hints full" \
            "VEIL_HINTS= VEIL_TEST_KDF=fast '$DEBUG_BIN' config --hints off"
        printf '[观察] 并发写入后的配置尾部:\n'
        tail -n 40 "$PHASE_HOME/.veil/config.toml"
    else
        skip_case "CFG-17" "并发测试未启用"
    fi
}

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

run_corruption_section() {
    section "DATA/REC：元数据、密文损坏和恢复"
    phase "corruption"

    run_case "DATA-init" 0 "损坏测试容器初始化" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" init corrupt
    run_case "DATA-add-good" 0 "添加正常文件" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" add corrupt "$FIXTURES/hello.txt" good.txt
    WS=$(resolve_workspace_for_link "$PHASE_WORK/corrupt.veil-link")
    meta="$WS/.veil-meta"
    cp "$meta" "$RUN_ROOT/meta-good.bin"
    meta_before=$(hash_file "$meta")

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

    first_enc=$(find "$WS" -name '*.enc' | LC_ALL=C sort | head -n 1)
    cp "$first_enc" "$RUN_ROOT/enc-good.bin"
    run_case "DATA-13-cipher-bitflip" 1 "拒绝被篡改的密文" \
        sh -c "cp '$RUN_ROOT/enc-good.bin' '$first_enc'; printf '\\001' | dd of='$first_enc' bs=1 seek=0 conv=notrunc 2>/dev/null; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex corrupt good.txt '$RUN_ROOT/corrupt-export.bin'"
    assert_file_missing "DATA-13-no-output" "corrupt ciphertext export leaves no output" "$RUN_ROOT/corrupt-export.bin"
    cp "$RUN_ROOT/enc-good.bin" "$first_enc"
    run_case "DATA-15-cipher-truncate" 1 "拒绝截断的密文" \
        sh -c "cp '$RUN_ROOT/enc-good.bin' '$first_enc'; size=\$(wc -c < '$RUN_ROOT/enc-good.bin' | tr -d ' '); new=\$((size - 1)); dd if='$RUN_ROOT/enc-good.bin' of='$first_enc' bs=1 count=\$new 2>/dev/null; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex corrupt good.txt '$RUN_ROOT/truncated-export.bin'"
    cp "$RUN_ROOT/enc-good.bin" "$first_enc"

    run_case "DATA-16-missing-cipher" 1 "密文缺失时导出失败" \
        sh -c "cp '$first_enc' '$RUN_ROOT/enc-backup.bin'; rm '$first_enc'; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex corrupt good.txt '$RUN_ROOT/missing-cipher-export.bin'; rc=\$?; mv '$RUN_ROOT/enc-backup.bin' '$first_enc'; exit \$rc"

    run_case "REC-10-lost-config" 0 "配置丢失后直接访问工作区" \
        sh -c "mv '$PHASE_HOME/.veil/config.toml' '$RUN_ROOT/config-lost.toml'; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list '$WS'; rc=\$?; mv '$RUN_ROOT/config-lost.toml' '$PHASE_HOME/.veil/config.toml'; exit \$rc"
    run_case "REC-11-header-link" 0 "根据工作区元数据头重建链接" \
        sh -c "cp -R '$WS' '$RUN_ROOT/detached-workspace'; mv '$RUN_ROOT/detached-workspace' '$RUN_ROOT/detached-workspace-renamed'; VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' link '$RUN_ROOT/detached-workspace-renamed' --output '$RUN_ROOT/detached.veil-link'"
    run_case "REC-11-open" 0 "打开重建的脱离工作区链接" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/detached.veil-link"
}

run_e2e_section() {
    section "E2E：用户旅程和选项参数等价性"
    phase "e2e"

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

    run_case "E2E-03-multi-link" 0 "多链接工作流" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init multilink && VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' link multilink --output '$RUN_ROOT/other-link.veil-link' && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add '$RUN_ROOT/other-link.veil-link' '$FIXTURES/hello.txt' shared.txt && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' list multilink"

    run_case "E2E-04-backup" 0 "打包、移动、解包、导出备份工作流" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init backup && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add backup '$FIXTURES/binary.bin' && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack backup --output '$RUN_ROOT/backup.veil' && mkdir -p '$RUN_ROOT/moved' && mv '$RUN_ROOT/backup.veil' '$RUN_ROOT/moved/backup.veil' && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$RUN_ROOT/moved/backup.veil' --name restored-backup --link '$RUN_ROOT/restored-backup.veil-link' && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' ex '$RUN_ROOT/restored-backup.veil-link' binary.bin '$RUN_ROOT/backup-export.bin'"
    assert_file_equals "E2E-04-bytes" "backup export matches" "$RUN_ROOT/backup-export.bin" "$FIXTURES/binary.bin"

    run_case "E2E-05-password-migration" 0 "改密迁移工作流" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init migrate && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add migrate '$FIXTURES/hello.txt' && VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' passwd migrate && VEIL_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack migrate --output '$RUN_ROOT/migrated.veil' && VEIL_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$RUN_ROOT/migrated.veil' --name migrated --link '$RUN_ROOT/migrated.veil-link'"
    run_case "E2E-05-old-fails" 1 "迁移后旧密码失效" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/migrated.veil-link"
    run_case "E2E-05-new-works" 0 "新密码可打开迁移后的包" \
        env VEIL_PASSWORD=3 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list "$RUN_ROOT/migrated.veil-link"

    run_case "E2E-10-empty-lifecycle" 1 "空生命周期，末尾删除不存在文件预期失败" \
        sh -c "VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' init empty-e2e && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' pack empty-e2e --output '$RUN_ROOT/empty-e2e.veil' && VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' unpack '$RUN_ROOT/empty-e2e.veil' --name empty-e2e-copy --link '$RUN_ROOT/empty-e2e-copy.veil-link' && VEIL_PASSWORD=1 VEIL_NEW_PASSWORD=4 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' passwd empty-e2e-copy && VEIL_PASSWORD=4 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' rm empty-e2e-copy missing-file"
    printf '[观察] 空生命周期末尾删除不存在文件应正常失败；组合命令退出码见上方。\n'
}

run_release_section() {
    section "Release 构建：安全提示和核心生命周期"
    phase "release"

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
        env VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" unpack "$RUN_ROOT/release.veil" \
        --name release-copy --link "$PHASE_WORK/release-copy.veil-link"
    run_case "REL-06-export-copy" 0 "Release 导出解包副本" \
        env VEIL_PASSWORD=1 VEIL_HINTS=off "$RELEASE_BIN" ex release-copy release.txt "$RUN_ROOT/release-copy-export.txt"
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

run_external_section() {
    if [ "$RUN_EXTERNAL" -ne 1 ]; then
        section "PLAT：外置卷工作流"
        skip_case "INIT-27" "未提供外置卷"
        skip_case "E2E-07" "未提供外置卷"
        return
    fi

    section "PLAT：外置卷工作流"
    phase "external"
    EXT="$EXTERNAL_DIR/veil-scenario-$RUN_STAMP-$$"
    mkdir -p "$EXT"
    note "External test directory: $EXT"

    run_case "INIT-27-external" 0 "外置卷初始化自动采用便携布局" \
        env VEIL_PASSWORD=1 VEIL_HINTS=full VEIL_TEST_KDF=fast "$DEBUG_BIN" init external \
        --link "$EXT/external.veil-link"
    assert_file_exists "INIT-27-link" "外置卷链接存在" "$EXT/external.veil-link"
    assert_file_exists "INIT-27-portable" "外置卷隐藏工作区存在" "$EXT/.veil/workspaces/default"

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
    run_case "RES-22-local-pointer" 0 "创建本地卷上的外置工作区链接" \
        env VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" link "$EXT/external.veil-link" \
        --output "$RUN_ROOT/external-local-pointer.veil-link"
    EXTERNAL_WS=$(find "$EXT/.veil/workspaces/default" -maxdepth 1 -type d -name 'veil-*' | head -n 1)
    EXTERNAL_LINK_HASH_BEFORE=$(hash_file "$RUN_ROOT/external-local-pointer.veil-link")
    EXTERNAL_ENC_BEFORE=$(count_enc_files "$EXTERNAL_WS")

    printf '\n[外置卷] 用户手动卸载前的目录布局:\n'
    find "$EXT" -maxdepth 4 -print | LC_ALL=C sort | sed -n '1,200p'

    if [ "$NON_INTERACTIVE" -eq 0 ]; then
        printf '\n将进行 %s 次“卸载外置卷 -> 验证不可用 -> 重新挂载 -> 读取并写入”循环。\n' "$EXTERNAL_CYCLES"
        cycle=1
        completed_cycles=0
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

            completed_cycles=$cycle
            cycle=$((cycle + 1))
        done

        if [ "$completed_cycles" -gt 0 ]; then
            EXPECTED_EXTERNAL_ENC=$((EXTERNAL_ENC_BEFORE + completed_cycles))
            assert_values_equal "EXTC-06-count" "多轮重连后新增密文数量正确" \
                "$EXPECTED_EXTERNAL_ENC" "$(count_enc_files "$EXTERNAL_WS")"
            assert_values_equal "EXTC-07-link-hash" "多轮断连重连后链接哈希保持不变" \
                "$EXTERNAL_LINK_HASH_BEFORE" "$(hash_file "$RUN_ROOT/external-local-pointer.veil-link")"
            if ACTUAL_EXTERNAL_B3=$(blake3_hex "$EXTERNAL_WS/.veil-meta" 2>/dev/null); then
                assert_true "EXTC-07-b3sum" "重连后元数据 BLAKE3 可重新计算" \
                    test -n "$ACTUAL_EXTERNAL_B3"
            else
                skip_case "EXTC-07-b3sum" "无法重算元数据 BLAKE3"
            fi
        fi
    else
        skip_case "EXTC" "非交互模式无法执行真实卸载和重挂载"
    fi

    printf '\n[观察] 外置测试目录会在最终清理阶段删除。\n'
    if [ "$KEEP_DATA" -eq 0 ]; then
        rm -rf "$EXT"
    fi
}

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
        run_case "CON-10-100-files" 0 "添加包含 100 个小文件的目录" \
            sh -c "mkdir -p '$RUN_ROOT/many'; i=1; while [ \$i -le 100 ]; do printf 'file-%s\\n' \"\$i\" > '$RUN_ROOT/many/f'\"\$i\".txt; i=\$((i+1)); done; VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast '$DEBUG_BIN' add concurrent '$RUN_ROOT/many' many"
        run_case "CON-10-verify" 0 "添加 100 个文件后列表验证" \
            env VEIL_PASSWORD=1 VEIL_HINTS=off VEIL_TEST_KDF=fast "$DEBUG_BIN" list concurrent
    fi

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

create_single_file_helper() {
    helper_root=$1
    mkdir -p "$helper_root/src"
    cat > "$helper_root/src/main.rs" <<'EOF'
use veil_core::container::Container;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: helper <output.veil> <password>");
        std::process::exit(2);
    }
    let mut container = Container::create(&args[1], args[2].clone(), "veil-scenario/1.0")
        .expect("create single-file container");
    container
        .add_file("secret.txt", b"veil brute force test\n")
        .expect("add test file");
    println!("created {}", args[1]);
}
EOF

    rlib=$(ls -t "$PROJECT_ROOT"/target/debug/deps/libveil_core-*.rlib 2>/dev/null | head -n 1)
    if [ -z "$rlib" ]; then
        printf '找不到已构建的 veil-core rlib\n' >&2
        return 1
    fi

    rustc --edition=2024 \
        "$helper_root/src/main.rs" \
        -L "dependency=$PROJECT_ROOT/target/debug/deps" \
        --extern "veil_core=$rlib" \
        -o "$helper_root/scenario-helper" || return 1

    HOME="$ORIGINAL_HOME" VEIL_TEST_KDF=fast "$helper_root/scenario-helper" "$2" "$3"
}

run_brute_force_section() {
    if [ "$RUN_BRUTE" -ne 1 ]; then
        section "BF：密码强度测试工具"
        skip_case "BF" "破解工具场景未启用"
        return
    fi

    section "BF：密码强度测试工具"
    phase "brute"
    RUN_BRUTE=1

    HELPER_ROOT="$RUN_ROOT/brute-helper"
    SINGLE="$RUN_ROOT/single-file.veil"
    if ! create_single_file_helper "$HELPER_ROOT" "$SINGLE" "1" || [ ! -s "$SINGLE" ]; then
        ASSERT_FAIL_COUNT=$((ASSERT_FAIL_COUNT + 1))
        printf '[断言失败] BF 夹具无法创建单文件容器\n'
        skip_case "BF" "夹具创建失败，跳过交互式攻击场景"
        return
    fi
    assert_file_exists "BF-fixture" "single-file container created" "$SINGLE"

    run_case "BF-01-no-path" 0 "未提供路径时进入交互模式并由 q 退出" \
        sh -c "printf 'q\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN'"
    run_case "BF-02-missing-path" 0 "路径不存在后可重新输入" \
        sh -c "printf 'missing.veil\\nq\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN'"

    PACK_FIXTURE="$RUN_ROOT/not-a-single-file.veil"
    printf 'not a container\n' > "$PACK_FIXTURE"
    run_case "BF-02-corrupt-header" 0 "损坏头部后可重新输入" \
        sh -c "printf '$PACK_FIXTURE\\nq\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN'"

    run_case "BF-04-dictionary-hit" 0 "字典攻击命中密码 1" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-hit.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-05-dictionary-miss" 0 "字典攻击未命中并给出结果" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-miss.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-06-dictionary-empty" 0 "空字典处理" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-empty.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"

    run_case "BF-08-custom-charset" 0 "自定义数字字符集命中密码 1" \
        sh -c "printf '2\\n3\\n1\\n1\\ny\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-09-invalid-range" 0 "非法长度范围被修正并处理" \
        sh -c "printf '2\\n1\\n9\\n2\\nn\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-11-preset" 0 "预设数字攻击命中密码 1" \
        sh -c "printf '3\\n1\\n1\\n1\\ny\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-12-word-combination" 0 "组词攻击命中密码 1" \
        sh -c "printf '4\\n1 2\\n1\\n1\\ny\\ny\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-14-thread-zero" 0 "线程数 0 被修正后处理" \
        sh -c "printf '5\\n0\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-16-switch-container" 0 "切换容器菜单流程" \
        sh -c "printf '6\\n$SINGLE\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-18-history" 0 "历史记录写入并显示" \
        sh -c "printf '1\\n$RUN_ROOT/wordlist-miss.txt\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"
    run_case "BF-20-retry-cancel" 0 "可取消重试历史密码" \
        sh -c "printf '7\\nn\\n0\\n' | HOME='$PHASE_HOME' VEIL_TEST_KDF=fast '$BRUTE_BIN' '$SINGLE'"

    printf '\n[BF 历史记录]\n'
    find "$PHASE_HOME/.veil_history" -type f -maxdepth 1 -print -exec sed -n '1,80p' {} \; 2>/dev/null || true
}

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
