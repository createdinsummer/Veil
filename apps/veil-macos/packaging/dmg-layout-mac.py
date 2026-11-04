"""DMG 的 Finder 布局：左侧应用，右侧安装目标。"""
application = defines["app"]
files = [application]
symlinks = {"Applications": "/Applications"}
format = "UDZO"
filesystem = "HFS+"
background = defines["background"]
icon = defines["icon"]
hide_extension = ["Veil.app"]

window_rect = ((160, 160), (640, 360))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
include_icon_view_settings = True
include_list_view_settings = False

arrange_by = None
grid_offset = (0, 0)
grid_spacing = 100
scroll_position = (0, 0)
label_pos = "bottom"
text_size = 12
icon_size = 88
icon_locations = {"Veil.app": (170, 190), "Applications": (470, 190)}
