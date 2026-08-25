# OpenLogi Frontmost Window — KWin script

KWin (KDE Plasma) does not let ordinary clients see which window is focused on
Wayland. It is not wlroots-based, so it implements no `wlr-foreign-toplevel`,
and its own `org.kde.KWin` D-Bus interface offers no non-interactive way in
either: `getWindowInfo` needs a window UUID that nothing hands out, and
`queryWindowInfo` asks the user to click a window. This minimal script bridges
that gap so OpenLogi's `kwin-script` frontmost backend can drive per-app
mouse-profile switching.

It reads only `workspace.activeWindow.resourceClass` — the same string X11
reports as `WM_CLASS`, so per-app profile keys stay consistent across X11,
XWayland and Plasma Wayland. No titles, no window contents, no input, no UI.

## Direction of travel

This runs the opposite way from the GNOME Shell extension. A KWin script can
*call* D-Bus but cannot *export* a service, so **OpenLogi serves and the script
pushes**: on every `workspace.windowActivated` the script calls

- name: `org.openlogi.KWinFrontmost`
- path: `/org/openlogi/KWinFrontmost`
- method: `SetFocusedWindowClass(s)` (empty string when nothing is focused)

OpenLogi caches the last value and reports it as the frontmost app. The backend
claims that name whenever the session bus is reachable and reports "no
frontmost app" until the first push, so a Plasma session without this script
installed keeps working exactly as it does today.

## Install

```sh
UUID=openlogi-frontmost
DEST="$HOME/.local/share/kwin/scripts/$UUID"
mkdir -p "$DEST"
cp -r "$UUID"/. "$DEST"/
```

Then enable it and make KWin pick it up:

```sh
kwriteconfig6 --file kwinrc --group Plugins --key "${UUID}Enabled" true
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.start
```

The script is also listed under *System Settings → Window Management → KWin
Scripts* once installed, where it can be toggled without the command line.

## Verify

With OpenLogi's hook running (so the bus name is claimed), load the script and
watch the backend follow the focus:

```sh
cargo run --example frontmost_app -p openlogi-hook
```

Switching windows should print each newly focused window's class. If it keeps
printing `(none …)`, check that the name is actually served:

```sh
busctl --user list | grep org.openlogi.KWinFrontmost
```

An owner there but no updates means the script is not loaded; no owner means
the hook is not running, and the script's pushes are being dropped.

## Notes

- KWin 6 (Plasma 6) and newer. `metadata.json` declares
  `X-Plasma-API-Minimum-Version: 6.0`; the API used here
  (`workspace.activeWindow`, `workspace.windowActivated`, `callDBus`) is the
  Plasma 6 scripting API.
- The script and the D-Bus name (`org.openlogi.*`) are placeholders that should
  track the project's namespace; if they change, update the matching constants
  in `crates/openlogi-hook/src/linux/kwin_script.rs`.
- The D-Bus method is `SetFocusedWindowClass`, not `setFocusedWindowClass`:
  zbus exports Rust's `set_focused_window_class` in PascalCase, and `callDBus`
  matches the member name literally.
