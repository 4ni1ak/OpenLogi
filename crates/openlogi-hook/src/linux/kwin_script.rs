//! Frontmost backend for KWin (KDE Plasma), fed by a companion KWin script.
//!
//! KWin is not wlroots-based, so it implements no `wlr-foreign-toplevel`, and
//! its own `org.kde.KWin` D-Bus interface has no way to read the active window
//! without user interaction: `getWindowInfo` needs a window UUID that nothing
//! hands out, and `queryWindowInfo` asks the user to click a window. Verified
//! against KWin 6 on a live Plasma Wayland session — the whole reason this
//! backend exists rather than a direct D-Bus read.
//!
//! What KWin *does* offer is scripting: a script sees
//! `workspace.activeWindow.resourceClass` and can `callDBus` out. So the flow
//! runs the opposite way from the GNOME backend — there the shell serves and
//! OpenLogi polls; here **OpenLogi serves and the script pushes** on every
//! activation, because a KWin script can call D-Bus but cannot export a
//! service.
//!
//! The script lives in `kwin-script/` in this crate and must be installed and
//! enabled for this backend to be useful. The backend itself starts whenever
//! the session bus is reachable and simply reports `None` until the first push
//! arrives, so a Plasma session without the script keeps working exactly as it
//! does today.
//!
//! `resourceClass` is the same string X11 reports as `WM_CLASS`, so per-app
//! profile keys stay consistent across X11, XWayland and Plasma Wayland.

use std::sync::{Arc, Mutex, PoisonError};

use tracing::debug;
use zbus::blocking::connection::Builder;
use zbus::interface;

use super::FrontmostSource;

/// Bus name and object path the companion script pushes to. Distinct from the
/// GNOME extension's `org.openlogi.Frontmost`, which is a *served* interface
/// pointing the other way — sharing a name would let the two backends fight
/// over ownership on a session that somehow had both.
const DBUS_NAME: &str = "org.openlogi.KWinFrontmost";
const DBUS_PATH: &str = "/org/openlogi/KWinFrontmost";

/// The last class the script pushed. `None` until the first activation after
/// the script starts, and cleared when KWin reports focus leaving every window.
type Cached = Arc<Mutex<Option<String>>>;

/// The D-Bus object the KWin script calls.
struct Receiver {
    cached: Cached,
}

#[interface(name = "org.openlogi.KWinFrontmost")]
impl Receiver {
    /// Record the newly activated window's `resourceClass`.
    ///
    /// An empty string means KWin activated no window (desktop focus, or the
    /// last window closing), which is reported as "no frontmost app" rather
    /// than as an app literally named "".
    fn set_focused_window_class(&self, class: &str) {
        let value = (!class.is_empty()).then(|| class.to_owned());
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }
}

/// Frontmost backend fed by the companion KWin script.
pub(super) struct KWinScriptSource {
    cached: Cached,
    // Owning the connection is what keeps the bus name claimed and the object
    // served; nothing reads it again.
    _connection: zbus::blocking::Connection,
}

impl FrontmostSource for KWinScriptSource {
    fn frontmost_app_id(&self) -> Option<String> {
        self.cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn name(&self) -> &'static str {
        "kwin-script"
    }
}

/// Claim the bus name and serve the push endpoint, or `None` when the session
/// bus is unreachable or the name is already owned.
pub(super) fn candidate() -> Option<Box<dyn FrontmostSource>> {
    let cached: Cached = Arc::new(Mutex::new(None));
    let receiver = Receiver {
        cached: Arc::clone(&cached),
    };
    let connection = Builder::session()
        .and_then(|builder| builder.name(DBUS_NAME))
        .and_then(|builder| builder.serve_at(DBUS_PATH, receiver))
        .and_then(Builder::build)
        .map_err(|error| debug!(%error, "kwin-script: could not serve the push endpoint"))
        .ok()?;

    debug!("kwin-script: serving {DBUS_NAME} — waiting for the script to push");
    Some(Box::new(KWinScriptSource {
        cached,
        _connection: connection,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a receiver plus the cache it writes into, without touching D-Bus.
    fn receiver() -> (Receiver, Cached) {
        let cached: Cached = Arc::new(Mutex::new(None));
        (
            Receiver {
                cached: Arc::clone(&cached),
            },
            cached,
        )
    }

    fn read(cached: &Cached) -> Option<String> {
        cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[test]
    fn reports_nothing_until_the_script_pushes() {
        let (_receiver, cached) = receiver();
        assert_eq!(read(&cached), None);
    }

    #[test]
    fn records_the_pushed_window_class() {
        let (receiver, cached) = receiver();
        receiver.set_focused_window_class("org.kde.konsole");
        assert_eq!(read(&cached), Some("org.kde.konsole".to_owned()));
    }

    /// The script sends `""` when KWin activated no window — focus on the
    /// desktop, or the last window closing. That is "no frontmost app", not an
    /// app whose name happens to be blank, which would otherwise become a
    /// per-app profile key of `""`.
    #[test]
    fn treats_an_empty_class_as_no_frontmost_window() {
        let (receiver, cached) = receiver();
        receiver.set_focused_window_class("org.kde.konsole");
        receiver.set_focused_window_class("");
        assert_eq!(read(&cached), None);
    }

    /// Activations overwrite rather than accumulate — the cache is the *last*
    /// focused window, so switching back and forth stays correct.
    #[test]
    fn later_activations_replace_earlier_ones() {
        let (receiver, cached) = receiver();
        receiver.set_focused_window_class("org.kde.konsole");
        receiver.set_focused_window_class("org.kde.kcalc");
        assert_eq!(read(&cached), Some("org.kde.kcalc".to_owned()));
        receiver.set_focused_window_class("org.kde.konsole");
        assert_eq!(read(&cached), Some("org.kde.konsole".to_owned()));
    }
}
