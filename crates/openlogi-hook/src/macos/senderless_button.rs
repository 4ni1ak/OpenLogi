//! Physical-device attribution for macOS button events whose HID sender is zero.

use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::{PhantomData, PhantomPinned};

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFAllocatorRef, CFType, CFTypeRef, TCFType as _};
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::number::CFNumber;
use core_foundation::set::{CFSet, CFSetGetValues, CFSetRef};
use core_foundation::string::{CFString, CFStringRef};
use tracing::warn;

use crate::EventDevice;

macro_rules! opaque_hid_type {
    ($name:ident) => {
        #[repr(C)]
        struct $name {
            _data: [u8; 0],
            _marker: PhantomData<(*mut u8, PhantomPinned)>,
        }
    };
}
opaque_hid_type!(OpaqueHidManager);
opaque_hid_type!(OpaqueHidDevice);
opaque_hid_type!(OpaqueHidElement);
opaque_hid_type!(OpaqueHidValue);

type IOHIDManagerRef = *mut OpaqueHidManager;
type IOHIDDeviceRef = *mut OpaqueHidDevice;
type IOHIDElementRef = *mut OpaqueHidElement;
type IOHIDValueRef = *mut OpaqueHidValue;
type IOReturn = i32;

const IO_RETURN_SUCCESS: IOReturn = 0;
const HID_PAGE_GENERIC_DESKTOP: u32 = 0x01;
const HID_USAGE_MOUSE: u32 = 0x02;
const HID_PAGE_BUTTON: u32 = 0x09;

// `objc2-io-kit` binds these, but it is not an `openlogi-hook` dependency;
// every Core Foundation value they return is adopted by a `core-foundation`
// wrapper instead of being released by hand.
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: CFAllocatorRef, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: CFDictionaryRef);
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDManagerClose(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDManagerCopyDevices(manager: IOHIDManagerRef) -> CFSetRef;
    fn IOHIDDeviceGetProperty(device: IOHIDDeviceRef, key: CFStringRef) -> CFTypeRef;
    fn IOHIDDeviceCopyMatchingElements(
        device: IOHIDDeviceRef,
        matching: CFDictionaryRef,
        options: u32,
    ) -> CFArrayRef;
    fn IOHIDDeviceGetValue(
        device: IOHIDDeviceRef,
        element: IOHIDElementRef,
        value: *mut IOHIDValueRef,
    ) -> IOReturn;
    fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
    fn IOHIDValueGetIntegerValue(value: IOHIDValueRef) -> isize;
}

/// Long-lived manager used only for rare sender-less button transitions.
struct HidManager {
    manager: IOHIDManagerRef,
    /// The +1 reference from `IOHIDManagerCreate`; dropped after `Drop`
    /// closes the manager.
    _owner: CFType,
}

// SAFETY: IOHIDManager is a Core Foundation object that may be transferred
// between threads. Ownership is exclusive and every operation is serialized by
// the event-tap callback's RefCell, so it is never accessed concurrently.
unsafe impl Send for HidManager {}

impl HidManager {
    fn open() -> Result<Self, IOReturn> {
        // SAFETY: a null allocator selects the process default; zero options are documented.
        let manager = unsafe { IOHIDManagerCreate(std::ptr::null(), 0) };
        if manager.is_null() {
            return Err(-1);
        }
        // SAFETY: `manager` is a non-null +1 CF object owned from here on.
        let owner = unsafe { CFType::wrap_under_create_rule(manager.cast_const().cast()) };
        // SAFETY: `manager` is live; null matching means all HID devices.
        unsafe { IOHIDManagerSetDeviceMatching(manager, std::ptr::null()) };
        // SAFETY: `manager` is live and opened once with documented zero options.
        let result = unsafe { IOHIDManagerOpen(manager, 0) };
        if result != IO_RETURN_SUCCESS {
            return Err(result);
        }
        Ok(Self {
            manager,
            _owner: owner,
        })
    }

    fn pressed_devices(&self, button_number: i64) -> Vec<ButtonCandidate> {
        let Ok(usage) = u32::try_from(button_number + 1) else {
            return Vec::new();
        };
        // SAFETY: the manager stays open for `self`; Copy returns a +1 set or null.
        let devices = unsafe { IOHIDManagerCopyDevices(self.manager) };
        if devices.is_null() {
            return Vec::new();
        }
        // SAFETY: `devices` is a non-null +1 CFSet owned from here on.
        let devices: CFSet = unsafe { CFSet::wrap_under_create_rule(devices) };
        device_values(&devices)
            .into_iter()
            .filter_map(|device| candidate_for_button(device.cast_mut().cast(), usage))
            .collect()
    }
}

impl Drop for HidManager {
    fn drop(&mut self) {
        // SAFETY: the manager is live until `_owner` drops after this returns.
        let _ = unsafe { IOHIDManagerClose(self.manager, 0) };
    }
}

#[derive(Clone, Debug)]
struct ButtonCandidate {
    device: EventDevice,
    pressed: bool,
}

/// Resolves and caches device identity across a sender-less down/up pair.
pub(super) struct SenderlessButtonResolver {
    manager: Option<HidManager>,
    held_sources: HashMap<i64, EventDevice>,
    /// Set by [`Self::resolve_press`] when an ambiguous press evicts a
    /// previously cached attribution, read and cleared by
    /// [`Self::take_attribution_invalidated`]. See that method's doc for why
    /// this needs to outlive `resolve`'s own return value.
    attribution_invalidated: bool,
}

impl SenderlessButtonResolver {
    pub(super) fn new() -> Self {
        let manager = match HidManager::open() {
            Ok(manager) => Some(manager),
            Err(code) => {
                warn!(
                    code = format_args!("{code:#x}"),
                    "could not open IOHIDManager for sender-less button attribution"
                );
                None
            }
        };
        Self {
            manager,
            held_sources: HashMap::new(),
            attribution_invalidated: false,
        }
    }

    #[cfg(test)]
    pub(super) fn unavailable() -> Self {
        Self {
            manager: None,
            held_sources: HashMap::new(),
            attribution_invalidated: false,
        }
    }

    /// Whether the most recent [`Self::resolve`] call evicted a previously
    /// cached attribution because a competing device made its button
    /// ambiguous — distinct from an ordinary release removing its own cache
    /// entry. The corresponding release for the device that lost its
    /// attribution will itself arrive unattributed (see `resolve`'s
    /// no-sender-id release path) and never reach the code that would end a
    /// hold begun under it, so the runtime uses this to cancel that hold
    /// directly instead. Clears on read so a later, unrelated call doesn't
    /// see a stale `true`.
    pub(super) fn take_attribution_invalidated(&mut self) -> bool {
        std::mem::take(&mut self.attribution_invalidated)
    }

    /// Drop every cached attribution. The OS tap being disabled and
    /// re-enabled (`TapDisabledByTimeout`/`TapDisabledByUserInput`) can drop
    /// button-up events without this resolver ever seeing them, so a cached
    /// source would otherwise outlive the physical hold it was attributed to
    /// and later mis-attribute an unrelated button's release.
    pub(super) fn cancel_all(&mut self) {
        self.held_sources.clear();
    }

    pub(super) fn resolve(
        &mut self,
        button_number: i64,
        pressed: bool,
        sender_source: Option<EventDevice>,
    ) -> Option<EventDevice> {
        if let Some(source) = sender_source {
            if !pressed {
                self.held_sources.remove(&button_number);
            }
            return Some(source);
        }
        if !pressed {
            return self.held_sources.remove(&button_number);
        }
        let candidates = self.manager.as_ref()?.pressed_devices(button_number);
        self.resolve_press(button_number, &candidates)
    }

    /// The new-press half of [`Self::resolve`], taking already-read
    /// candidates directly so it can be exercised without a live
    /// `IOHIDManager` — see the tests module.
    fn resolve_press(
        &mut self,
        button_number: i64,
        candidates: &[ButtonCandidate],
    ) -> Option<EventDevice> {
        let Some(source) = unique_pressed_logitech(candidates) else {
            // An ambiguous new press (e.g. a second mouse pressing the same
            // button number while a prior press is still held) invalidates
            // any stale cache entry for this button: nothing proves a later
            // release belongs to the device that earned the cached
            // attribution rather than to this new, unattributable press.
            // Without this, that release would wrongly end the cached
            // device's hold instead of passing through unattributed like its
            // own down did.
            if self.held_sources.remove(&button_number).is_some() {
                self.attribution_invalidated = true;
            }
            return None;
        };
        self.held_sources.insert(button_number, source.clone());
        Some(source)
    }
}

/// The set's members, borrowed: they stay valid only while `devices` lives.
fn device_values(devices: &CFSet) -> Vec<*const c_void> {
    let mut values = vec![std::ptr::null(); devices.len()];
    // SAFETY: `devices` is live and `values` has exactly one slot per member.
    unsafe { CFSetGetValues(devices.as_concrete_TypeRef(), values.as_mut_ptr()) };
    values
}

fn candidate_for_button(device: IOHIDDeviceRef, usage: u32) -> Option<ButtonCandidate> {
    if device_number(device, "PrimaryUsagePage")? != u64::from(HID_PAGE_GENERIC_DESKTOP)
        || device_number(device, "PrimaryUsage")? != u64::from(HID_USAGE_MOUSE)
    {
        return None;
    }
    let pressed = button_value(device, usage)? != 0;
    Some(ButtonCandidate {
        device: EventDevice {
            vendor_id: property_u32(device, "VendorID"),
            product_id: property_u32(device, "ProductID"),
            product_name: device_string(device, "Product"),
        },
        pressed,
    })
}

fn button_value(device: IOHIDDeviceRef, usage: u32) -> Option<isize> {
    // SAFETY: `device` is retained by the copied device set; null matches all elements.
    let elements = unsafe { IOHIDDeviceCopyMatchingElements(device, std::ptr::null(), 0) };
    if elements.is_null() {
        return None;
    }
    // SAFETY: `elements` is a non-null +1 CFArray owned from here on.
    let elements: CFArray = unsafe { CFArray::wrap_under_create_rule(elements) };
    elements.get_all_values().into_iter().find_map(|element| {
        let element: IOHIDElementRef = element.cast_mut().cast();
        // SAFETY: `element` is retained by `elements`, which outlives this closure.
        let matches = unsafe {
            IOHIDElementGetUsagePage(element) == HID_PAGE_BUTTON
                && IOHIDElementGetUsage(element) == usage
        };
        if !matches {
            return None;
        }
        current_value(device, element)
    })
}

fn current_value(device: IOHIDDeviceRef, element: IOHIDElementRef) -> Option<isize> {
    let mut value = std::ptr::null_mut();
    // SAFETY: device and element belong to the live copied device set/element array.
    let result = unsafe { IOHIDDeviceGetValue(device, element, &raw mut value) };
    if result != IO_RETURN_SUCCESS || value.is_null() {
        return None;
    }
    // SAFETY: a successful read returned a live value borrowed from IOHIDDevice.
    Some(unsafe { IOHIDValueGetIntegerValue(value) })
}

fn property_u32(device: IOHIDDeviceRef, key: &str) -> Option<u32> {
    device_number(device, key).and_then(|value| u32::try_from(value).ok())
}

/// Read a `CFString`-typed HID device property, e.g. `"Product"` — the same
/// key [`crate::EventDevice::is_trackpad_like`] matches on. Without this, a
/// candidate built here always carries `product_name: None`, so a Logitech
/// touchpad exposing a mouse HID interface would pass the trackpad check by
/// omission and become remappable through `is_logitech()` alone.
fn device_string(device: IOHIDDeviceRef, key: &str) -> Option<String> {
    Some(
        device_property(device, key)?
            .downcast::<CFString>()?
            .to_string(),
    )
}

fn device_number(device: IOHIDDeviceRef, key: &str) -> Option<u64> {
    let value = device_property(device, key)?
        .downcast::<CFNumber>()?
        .to_i64()?;
    u64::try_from(value).ok()
}

fn device_property(device: IOHIDDeviceRef, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    // SAFETY: `device` is live and `key` is a valid CFString for this call.
    let property = unsafe { IOHIDDeviceGetProperty(device, key.as_concrete_TypeRef()) };
    if property.is_null() {
        return None;
    }
    // SAFETY: a non-null Get-rule CF object; this takes its own retain.
    Some(unsafe { CFType::wrap_under_get_rule(property) })
}

fn unique_pressed_logitech(candidates: &[ButtonCandidate]) -> Option<EventDevice> {
    let mut pressed = candidates.iter().filter(|candidate| candidate.pressed);
    let source = pressed.next()?;
    if pressed.next().is_some() || !source.device.is_logitech() {
        return None;
    }
    Some(source.device.clone())
}

#[cfg(test)]
mod tests;
