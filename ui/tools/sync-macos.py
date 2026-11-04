"""将设计参数同步到原生客户端资源，不依赖 Node.js。"""
import pathlib
import sys

root = pathlib.Path(__file__).resolve().parents[2]
source = root / "ui/tokens.json"
target = root / "apps/veil-macos/Sources/VeilMac/Resources/DesignTokens.json"
content = source.read_bytes()
if "--check" in sys.argv:
    if not target.exists() or target.read_bytes() != content:
        sys.exit("macOS 设计参数未同步，请运行 python3 ui/tools/sync-macos.py")
elif not target.exists() or target.read_bytes() != content:
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(content)
