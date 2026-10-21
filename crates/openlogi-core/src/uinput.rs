//! How to recognize the Linux uinput devices OpenLogi creates, by name.
//!
//! Pure string matching, so it lives here rather than beside the uinput
//! creators in `openlogi-inject`: the hook must never grab these devices, and
//! the permissions probe must never count them as nodes the user needs access
//! to, and the probe's crate should not link evdev and zbus to know that.
//!
//! The name is the one identity a pass-through mouse does not mirror from the
//! physical device it re-injects — bus, vendor, product, version, and the
//! capability bits all are copied, so only the name tells the two apart.

/// Prefix of every uinput device name OpenLogi creates.
const PREFIX: &str = "OL ";

/// Prefix the same devices carried in earlier releases. A development agent
/// can run next to an older installed agent and must not grab its devices.
const LEGACY_PREFIX: &str = "OpenLogi ";

/// Whether `name` belongs to a uinput device that OpenLogi created, in this
/// release or an earlier one.
#[must_use]
pub fn is_own_device(name: &str) -> bool {
    name.starts_with(PREFIX) || name.starts_with(LEGACY_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_from_earlier_releases_are_recognized_as_own() {
        assert!(is_own_device("OpenLogi virtual mouse"));
        assert!(is_own_device("OpenLogi action injector"));
    }

    #[test]
    fn physical_and_foreign_devices_are_not_own() {
        for name in [
            "Logitech MX Master 3S",
            "MX Master 3S",
            "Logitech USB Receiver Mouse",
            "OLED Touch Panel",
            "OL",
            "",
        ] {
            assert!(!is_own_device(name), "{name:?} is not an OpenLogi device");
        }
    }
}
