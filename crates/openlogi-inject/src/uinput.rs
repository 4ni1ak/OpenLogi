//! The uinput devices OpenLogi creates on Linux, and how to recognize them.
//!
//! Every uinput creator and the Linux hook must make two decisions the same
//! way, so both live here. Each device name carries one private prefix that
//! [`is_own_device`] matches, so the hook never grabs a device OpenLogi
//! created. Each name also avoids the words GNOME Shell's X11 backend reads
//! as a device type.
//!
//! That backend (mutter 48 and earlier, `create_device` in
//! `src/backends/x11/meta-seat-x11.c`) guesses the type of a device that
//! reports none from its lowercased name. A name that contains "pen" or
//! "wacom" becomes a stylus, and mutter then applies the stylus button map,
//! which swaps the middle and right buttons (#1842). "eraser", "cursor",
//! " pad", and "touchpad" select other tablet or touchpad types. "OpenLogi"
//! contains "pen", so no device name can carry the product name.

use std::io;

use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
// Shared with the permissions probe, which must not link evdev to use it.
pub use openlogi_core::uinput::is_own_device;

/// A uinput device OpenLogi creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Device {
    /// The pass-through mouse that re-injects the events of one grabbed mouse.
    PassThroughMouse,
    /// The device that synthesizes the keys, clicks, and scrolls of bound
    /// actions.
    ActionInjector,
}

impl Device {
    /// Every variant, so the naming tests cannot miss a new device.
    #[cfg(test)]
    const ALL: [Self; 2] = [Self::PassThroughMouse, Self::ActionInjector];

    /// The name the device registers with the kernel, and that libinput, the
    /// display server, and `xinput` show.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PassThroughMouse => "OL virtual mouse",
            Self::ActionInjector => "OL action injector",
        }
    }
}

/// Start a uinput builder for `device` that already carries its name.
///
/// # Errors
///
/// Returns the error from opening `/dev/uinput`.
pub fn builder(device: Device) -> io::Result<VirtualDeviceBuilder<'static>> {
    Ok(VirtualDevice::builder()?.name(device.name()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The substrings mutter 48's X11 backend looks for in a lowercased
    /// device name to choose a tablet or touchpad type.
    const MUTTER_TYPE_WORDS: [&str; 6] = ["eraser", "cursor", " pad", "wacom", "pen", "touchpad"];

    #[test]
    fn every_device_name_is_recognized_as_own() {
        for device in Device::ALL {
            assert!(
                is_own_device(device.name()),
                "{device:?} ({:?}) would be grabbed by the hook",
                device.name()
            );
        }
    }

    #[test]
    fn no_device_name_reads_as_a_tablet_or_touchpad_to_mutter() {
        for device in Device::ALL {
            let name = device.name().to_ascii_lowercase();
            for word in MUTTER_TYPE_WORDS {
                assert!(
                    !name.contains(word),
                    "{device:?} ({name:?}) contains {word:?}, which GNOME Shell on X11 \
                     reads as a tablet or touchpad"
                );
            }
        }
    }
}
