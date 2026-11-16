"""
同步 Web 设计 token 到原生 macOS 资源文件。

所属模块：ui 设计基础设施，保证原生应用读取的 JSON 与 Web 唯一源文件一致。
核心数据：source 是 ui/tokens.json；target 是 macOS 包内 Resources/DesignTokens.json；content 为源字节。
核心流程：--check 模式只比较内容并报告不同步；普通模式仅在缺失或内容变化时创建目录并写入。
依赖能力：Python pathlib、字节文件读写和命令行参数。整体目标是让 Web 与 macOS 共享相同设计参数。
"""
import pathlib
import sys

# 从脚本位置推导仓库根目录，避免依赖运行命令时的当前目录。
root = pathlib.Path(__file__).resolve().parents[2]
# 共享设计参数源路径；脚本复制原始字节，不验证 JSON 语法或每个 token 的业务合法性。
source = root / "ui/tokens.json"
# SwiftPM 会把此资源打包到 Bundle.module；普通同步模式可能改写已跟踪的工作区文件。
target = root / "apps/veil-macos/Sources/VeilMac/Resources/DesignTokens.json"
# 源字节完整读入内存；文件缺失、权限不足等 OSError 直接传播并以非零状态结束，不使用旧目标兜底。
# 按原始字节比较，任何格式、空白和换行差异都会判为不同步。
content = source.read_bytes()
# 参数列表中任意位置出现精确的 --check 就只读检查；没有参数解析器，其他参数被忽略，不提供单独帮助分支。
# 目标缺失或字节不同时用文本 SystemExit 返回非零；相同时正常结束，没有额外成功输出。
if "--check" in sys.argv:
    if not target.exists() or target.read_bytes() != content:
        sys.exit("macOS 设计参数未同步，请运行 python3 ui/tools/sync-macos.py")
# 普通模式仅在目标缺失或不同步时创建父目录并覆盖写入，内容相同则保留目标。
# 写入不是原子替换，也没有并发锁；中断或写入失败可能留下不完整文件，再次执行可重新同步。
elif not target.exists() or target.read_bytes() != content:
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(content)
