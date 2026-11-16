"""配置 macOS DMG 在 Finder 中的窗口外观。
所属模块：桌面安装包布局；定义 Veil.app、Applications 快捷方式、背景图和图标位置。
变量由 dmgbuild 构建上下文提供；目标是在打开镜像时直观呈现“拖动应用到 Applications”的安装步骤。"""
# dmgbuild 注入的应用包路径；此对象会作为可拖动安装项放入镜像。
application = defines["app"]
# Finder 窗口中展示的安装项列表；目前只显示 Veil.app。
files = [application]
# 镜像内的 Applications 图标链接到系统应用目录，作为安装目标。
symlinks = {"Applications": "/Applications"}
# 镜像输出采用 UDZO 压缩只读格式；这是 dmgbuild 的格式参数，不是应用数据加密设置。
format = "UDZO"
# 镜像内部文件系统为 HFS+，用于保存安装项及 Finder 布局元数据。
filesystem = "HFS+"
# 由 dmgbuild defines 注入的背景图路径；脚本只引用，不生成或检查图片。
background = defines["background"]
# 由构建上下文注入的镜像图标路径，不是应用包内图标的重新绘制入口。
icon = defines["icon"]
# 需要在 Finder 隐藏扩展名的项目名称集合；每项对应镜像内的显示条目。
hide_extension = ["Veil.app"]

# Finder 窗口在屏幕上的位置和尺寸；图标坐标按此窗口布局。
window_rect = ((160, 160), (640, 360))
# 打开镜像时默认使用图标视图，便于按预设坐标展示拖动安装路径。
default_view = "icon-view"
# 隐藏 Finder 状态栏，减少安装窗口内的额外信息。
show_status_bar = False
# 隐藏 Finder 标签页栏。
show_tab_view = False
# 隐藏工具栏，为安装插画保留空间。
show_toolbar = False
# 隐藏底部路径栏。
show_pathbar = False
# 隐藏系统侧栏，突出当前镜像的安装项。
show_sidebar = False
# 关闭按文件内容生成的图标预览，保持应用和 Applications 图标外观。
show_icon_preview = False
# 写入图标视图设置，使图标大小和坐标随镜像分发。
include_icon_view_settings = True
# 不额外生成列表视图布局配置。
include_list_view_settings = False

# 不启用按名称等字段自动排列，以保留手动指定的安装项坐标。
arrange_by = None
# 图标网格原点偏移（横向、纵向）；当前不偏移。
grid_offset = (0, 0)
# Finder 图标网格间距，和下方明确指定的图标坐标共同组成布局设置。
grid_spacing = 100
# 打开时滚动偏移（横向、纵向）为零，显示窗口起始区域。
scroll_position = (0, 0)
# 文件名称显示在图标下方。
label_pos = "bottom"
# Finder 图标文字字号。
text_size = 12
# Finder 安装项图标尺寸。
icon_size = 88
# 条目名称到窗口内 (x, y) 坐标的映射；Veil.app 在左、Applications 在右，提示拖动安装。
icon_locations = {"Veil.app": (170, 190), "Applications": (470, 190)}
