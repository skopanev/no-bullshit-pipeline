# dmgbuild settings — deterministic DMG layout (no Finder AppleScript).
#
# create-dmg drove the window via a Finder AppleScript that, on recent macOS,
# intermittently failed to apply icon size + background before unmount → tiny
# icons, white background. dmgbuild writes the .DS_Store directly, so the layout
# is reproducible. Values mirror the old create-dmg invocation so the icons land
# on the background arrow as designed.
#
# Invoke:  dmgbuild -s dmg-settings.py -D app=/path/to/nbp.app "nbp" out.dmg

import os.path

app_path = defines.get("app", "src-tauri/target/release/bundle/macos/nbp.app")
appname = os.path.basename(app_path)

# --- volume ---------------------------------------------------------------
format = "UDZO"  # compressed, same as create-dmg
files = [app_path]
symlinks = {"Applications": "/Applications"}
hide_extension = [appname]

# --- window / layout ------------------------------------------------------
background = "src-tauri/dmg-assets/background.png"  # 820x520, arrow baked in
window_rect = ((200, 120), (820, 520))  # (x, y), (w, h) — matches the background
default_view = "icon-view"
icon_size = 160
text_size = 14
icon_locations = {
    appname: (220, 230),
    "Applications": (600, 230),
}
