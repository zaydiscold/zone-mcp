# How Zone.dmg looks when it is opened: read by dmgbuild (see scripts/build-app.sh).
# The icon positions must match the arrow drawn by scripts/make-dmg-background.swift.
import os

app = defines["app"]  # noqa: F821  (dmgbuild passes -D app=/path/to/Zone.app)
here = defines["here"]  # noqa: F821

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = os.path.join(app, "Contents", "Resources", "AppIcon.icns")
background = os.path.join(here, "background.png")

window_rect = ((240, 160), (640, 420))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
include_icon_view_settings = "auto"
include_list_view_settings = "auto"

arrange_by = None
grid_offset = (0, 0)
grid_spacing = 100
scroll_position = (0, 0)
label_pos = "bottom"
text_size = 13
icon_size = 112
icon_locations = {
    "Zone.app": (170, 205),
    "Applications": (470, 205),
}
