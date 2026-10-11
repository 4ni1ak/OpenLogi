//! Physical-device attribution for macOS button events whose HID sender is zero.

use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::{PhantomData, PhantomPinned};

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFAllocatorRef, CFType, CFTypeRef, TCFType as _};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::runloop::{CFRunLoop, CFRunLoopRef, kCFRunLoopDefaultMode};
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
    fn IOHIDManagerScheduleWithRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
    fn IOHIDManagerUnscheduleFromRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
    );
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
    /// The event-tap thread's run loop, which delivers the manager's device
    /// arrival and removal notifications; without it the device set stays
    /// as it was at open, and an unplugged mouse lingers there unreadable.
    run_loop: CFRunLoop,
    /// The +1 reference from `IOHIDManagerCreate`; dropped after `Drop`
    /// closes the manager.
    _owner: CFType,
}

// SAFETY: IOHIDManager is a Core Foundation object that may be transferred
// between threads. Ownership is exclusive and every operation is serialized by
// the event-tap callback's RefCell, so it is never accessed concurrently. It is
// created, scheduled, used and dropped on the event-tap thread.
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
        let matching = CFDictionary::from_CFType_pairs(&[
            (
                CFString::from_static_string("DeviceUsagePage"),
                CFNumber::from(i64::from(HID_PAGE_GENERIC_DESKTOP)),
            ),
            (
                CFString::from_static_string("DeviceUsage"),
                CFNumber::from(i64::from(HID_USAGE_MOUSE)),
            ),
        ]);
        // SAFETY: `manager` is live; the manager copies the matching dictionary.
        unsafe { IOHIDManagerSetDeviceMatching(manager, matching.as_concrete_TypeRef()) };
        let run_loop = CFRunLoop::get_current();
        // SAFETY: `manager` and the run loop are live; the mode is a static
        // framework constant. Unscheduled in `Drop`.
        unsafe {
            IOHIDManagerScheduleWithRunLoop(
                manager,
                run_loop.as_concrete_TypeRef(),
                kCFRunLoopDefaultMode,
            );
        }
        let manager = Self {
            manager,
            run_loop,
            _owner: owner,
        };
        // SAFETY: `manager` is live and opened once with documented zero options.
        let result = unsafe { IOHIDManagerOpen(manager.manager, 0) };
        if result != IO_RETURN_SUCCESS {
            return Err(result);
        }
        Ok(manager)
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
        // SAFETY: the manager is live until `_owner` drops after this returns,
        // and was scheduled on `run_loop` in this mode by `open`.
        unsafe {
            IOHIDManagerUnscheduleFromRunLoop(
                self.manager,
                self.run_loop.as_concrete_TypeRef(),
                kCFRunLoopDefaultMode,
            );
            let _ = IOHIDManagerClose(self.manager, 0);
        }
    }
}

/// How the backing HID event identified the device behind a button event.
pub(super) enum ButtonSender {
    /// No HID event backs the `CGEvent`: software posted it, so it has no
    /// physical device to attribute and must not touch any held press.
    Posted,
    /// The HID sender resolved to this device.
    Device { sender_id: u64, device: EventDevice },
    /// A HID event with sender id zero, which macOS 27 reports for some
    /// physical button transitions.
    Unidentified,
}

impl ButtonSender {
    /// Classify `event_sender_id`'s result: `None` is a posted event, zero is
    /// a sender-less HID event, anything else names a device via `lookup`.
    pub(super) fn from_sender_id(
        sender_id: Option<u64>,
        lookup: impl FnOnce(u64) -> EventDevice,
    ) -> Self {
        match sender_id {
            None => Self::Posted,
            Some(0) => Self::Unidentified,
            Some(sender_id) => Self::Device {
                sender_id,
                device: lookup(sender_id),
            },
        }
    }
}

/// What a mouse's HID element reports for the queried button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonState {
    Pressed,
    Released,
    /// The device has the button but its state could not be read.
    Unknown,
}

#[derive(Clone, Debug)]
struct ButtonCandidate {
    device: EventDevice,
    state: ButtonState,
}

/// Every unreleased press of one button.
#[derive(Debug, Default)]
struct ButtonHolds {
    /// Identified devices holding the button, keyed by HID sender id.
    identified: HashMap<u64, EventDevice>,
    /// The sender-less press attributed by the `IOHIDManager` fallback.
    senderless: Option<EventDevice>,
    /// Sender-less presses refused attribution, each as the mice that may
    /// have made it: those otherwise unaccounted for that read pressed (or
    /// unreadable) when it arrived and still do.
    refused: Vec<Vec<EventDevice>>,
}

/// A sender-less transition's attribution, and whether it dropped a held
/// sender-less attribution whose hold the runtime must now cancel.
type Resolved = (Option<EventDevice>, bool);

impl ButtonHolds {
    fn is_empty(&self) -> bool {
        self.identified.is_empty() && self.senderless.is_none() && self.refused.is_empty()
    }

    /// Forget holders the poll no longer sees pressed: their release was
    /// lost, e.g. the mouse was unplugged mid-press, or is still queued.
    /// They only ever veto attribution, so dropping one early is harmless.
    fn prune(&mut self, candidates: &[ButtonCandidate]) {
        let may_hold = |device: &EventDevice| {
            matches!(
                polled_state(candidates, device),
                Some(ButtonState::Pressed | ButtonState::Unknown)
            )
        };
        self.identified.retain(|_, held| may_hold(held));
        for suspects in &mut self.refused {
            suspects.retain(|suspect| may_hold(suspect));
        }
        self.refused.retain(|suspects| !suspects.is_empty());
    }

    /// Clear `device` from every refused press: a device holds one press at
    /// most, and that one is accounted for elsewhere or just ended.
    fn disown(&mut self, device: &EventDevice) {
        for suspects in &mut self.refused {
            suspects.retain(|suspect| suspect != device);
        }
        self.refused.retain(|suspects| !suspects.is_empty());
    }

    /// Whether `device` certainly holds the button under a recorded press,
    /// so its pressed state proves nothing about a new one. A refused press
    /// with several suspects pins none of them: all but one may be pressing
    /// anew.
    fn accounts_for(&self, device: &EventDevice) -> bool {
        self.senderless.as_ref() == Some(device)
            || self.identified.values().any(|held| held == device)
            || self
                .refused
                .iter()
                .any(|suspects| matches!(suspects.as_slice(), [suspect] if suspect == device))
    }

    fn senderless_press(&mut self, candidates: Option<&[ButtonCandidate]>) -> Resolved {
        let Some(candidates) = candidates else {
            return (None, false);
        };
        self.prune(candidates);
        let mut invalidated = false;
        // The holder cannot press the button again, so while it may still
        // hold, this press is some other, possibly unseen, device's.
        let holder_may_hold = match &self.senderless {
            Some(held) => match polled_state(candidates, held) {
                Some(ButtonState::Pressed | ButtonState::Unknown) => true,
                // The holder's release was lost, e.g. it was unplugged.
                Some(ButtonState::Released) | None => {
                    self.senderless = None;
                    invalidated = true;
                    false
                }
            },
            None => false,
        };
        let unaccounted: Vec<_> = candidates
            .iter()
            .filter(|candidate| !self.accounts_for(&candidate.device))
            .cloned()
            .collect();
        let source = if holder_may_hold {
            None
        } else {
            unique_pressed_logitech(&unaccounted)
        };
        if source.is_some() {
            self.senderless.clone_from(&source);
        } else {
            let suspects: Vec<_> = unaccounted
                .into_iter()
                .filter(|candidate| candidate.state != ButtonState::Released)
                .map(|candidate| candidate.device)
                .collect();
            if !suspects.is_empty() {
                self.refused.push(suspects);
            }
        }
        (source, invalidated)
    }

    /// A sender-less release ends the attributed press only once its holder
    /// reads released; any other release ends a refused press, whose record
    /// the poll prunes once its suspects read released.
    fn senderless_release(
        &mut self,
        poll: impl FnOnce() -> Option<Vec<ButtonCandidate>>,
    ) -> Resolved {
        if self.is_empty() {
            return (None, false);
        }
        let candidates = poll();
        if let Some(candidates) = &candidates {
            self.prune(candidates);
        }
        let Some(held) = self.senderless.take() else {
            return (None, false);
        };
        match candidates
            .as_deref()
            .and_then(|candidates| polled_state(candidates, &held))
        {
            Some(ButtonState::Released) => (Some(held), false),
            Some(ButtonState::Pressed) => {
                self.senderless = Some(held);
                (None, false)
            }
            // Unreadable or unplugged: whose release this is is unknowable,
            // so the attributed hold is cancelled rather than guessed at.
            Some(ButtonState::Unknown) | None => (None, true),
        }
    }
}

/// Every button's unreleased presses, keyed by CoreGraphics button number.
#[derive(Debug, Default)]
struct HoldTracker {
    holds: HashMap<i64, ButtonHolds>,
    /// Set when a sender-less attribution is dropped without its release;
    /// read and cleared by
    /// [`SenderlessButtonResolver::take_attribution_invalidated`].
    attribution_invalidated: bool,
}

/// Resolves and caches device identity across a sender-less down/up pair.
pub(super) struct SenderlessButtonResolver {
    manager: Option<HidManager>,
    tracker: HoldTracker,
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
            tracker: HoldTracker::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn unavailable() -> Self {
        Self {
            manager: None,
            tracker: HoldTracker::default(),
        }
    }

    /// Whether the most recent [`Self::resolve`] call dropped a sender-less
    /// attribution without its release: the holder became unreadable, or
    /// the poll shows its release was lost (e.g. it was unplugged). That
    /// release will never be recognized, so the runtime uses this to cancel
    /// the hold begun under it directly. Clears on read so a later,
    /// unrelated call doesn't see a stale `true`.
    pub(super) fn take_attribution_invalidated(&mut self) -> bool {
        std::mem::take(&mut self.tracker.attribution_invalidated)
    }

    /// Drop every cached attribution. The OS tap being disabled and
    /// re-enabled (`TapDisabledByTimeout`/`TapDisabledByUserInput`) can drop
    /// button-up events without this resolver ever seeing them, so a cached
    /// source would otherwise outlive the physical hold it was attributed to
    /// and later mis-attribute an unrelated button's release.
    pub(super) fn cancel_all(&mut self) {
        self.tracker.holds.clear();
    }

    pub(super) fn resolve(
        &mut self,
        button_number: i64,
        pressed: bool,
        sender: ButtonSender,
    ) -> Option<EventDevice> {
        let Self { manager, tracker } = self;
        tracker.transition(button_number, pressed, sender, || {
            manager
                .as_ref()
                .map(|manager| manager.pressed_devices(button_number))
        })
    }
}

impl HoldTracker {
    /// [`SenderlessButtonResolver::resolve`] with the `IOHIDManager` read
    /// injected as `poll` (`None` when no manager is available), so it can be
    /// exercised without hardware — see the tests module.
    fn transition(
        &mut self,
        button_number: i64,
        pressed: bool,
        sender: ButtonSender,
        poll: impl FnOnce() -> Option<Vec<ButtonCandidate>>,
    ) -> Option<EventDevice> {
        let holds = self.holds.entry(button_number).or_default();
        let (device, invalidated) = match sender {
            ButtonSender::Posted => (None, false),
            ButtonSender::Device { sender_id, device } => {
                if pressed {
                    // The press a refused one's poll saw this device making
                    // is this one, delivered late, not the refused press.
                    holds.disown(&device);
                    holds.identified.insert(sender_id, device.clone());
                } else if holds.identified.remove(&sender_id).is_none() {
                    // No identified press to end, so this ends a sender-less one.
                    if holds.senderless.as_ref() == Some(&device) {
                        holds.senderless = None;
                    }
                    holds.disown(&device);
                }
                (Some(device), false)
            }
            ButtonSender::Unidentified if pressed => holds.senderless_press(poll().as_deref()),
            ButtonSender::Unidentified => holds.senderless_release(poll),
        };
        self.attribution_invalidated |= invalidated;
        if self
            .holds
            .get(&button_number)
            .is_some_and(ButtonHolds::is_empty)
        {
            self.holds.remove(&button_number);
        }
        device
    }
}

/// `device`'s polled state, or `None` when no polled mouse matches it. Any
/// matching mouse pressed counts as pressed, and any unreadable one as
/// unknown, so look-alike mice never make a hold look released.
fn polled_state(candidates: &[ButtonCandidate], device: &EventDevice) -> Option<ButtonState> {
    candidates
        .iter()
        .filter(|candidate| candidate.device == *device)
        .map(|candidate| candidate.state)
        .reduce(|state, other| match (state, other) {
            (ButtonState::Unknown, _) | (_, ButtonState::Unknown) => ButtonState::Unknown,
            (ButtonState::Pressed, _) | (_, ButtonState::Pressed) => ButtonState::Pressed,
            (ButtonState::Released, ButtonState::Released) => ButtonState::Released,
        })
}

/// The set's members, borrowed: they stay valid only while `devices` lives.
fn device_values(devices: &CFSet) -> Vec<*const c_void> {
    let mut values = vec![std::ptr::null(); devices.len()];
    // SAFETY: `devices` is live and `values` has exactly one slot per member.
    unsafe { CFSetGetValues(devices.as_concrete_TypeRef(), values.as_mut_ptr()) };
    values
}

/// The device's state for button `usage`, or `None` when it has no such
/// button. The manager only opens devices exposing a mouse usage, so every
/// device here is a potential source; any read that fails reports
/// [`ButtonState::Unknown`] instead of dropping the device.
fn candidate_for_button(device: IOHIDDeviceRef, usage: u32) -> Option<ButtonCandidate> {
    let state = button_state(device, usage)?;
    Some(ButtonCandidate {
        device: EventDevice {
            vendor_id: property_u32(device, "VendorID"),
            product_id: property_u32(device, "ProductID"),
            product_name: device_string(device, "Product"),
        },
        state,
    })
}

fn button_state(device: IOHIDDeviceRef, usage: u32) -> Option<ButtonState> {
    // SAFETY: `device` is retained by the copied device set; null matches all elements.
    let elements = unsafe { IOHIDDeviceCopyMatchingElements(device, std::ptr::null(), 0) };
    if elements.is_null() {
        return Some(ButtonState::Unknown);
    }
    // SAFETY: `elements` is a non-null +1 CFArray owned from here on.
    let elements: CFArray = unsafe { CFArray::wrap_under_create_rule(elements) };
    let mut state = None;
    for element in elements.get_all_values() {
        let element: IOHIDElementRef = element.cast_mut().cast();
        // SAFETY: `element` is retained by `elements`, which outlives this loop.
        let matches = unsafe {
            IOHIDElementGetUsagePage(element) == HID_PAGE_BUTTON
                && IOHIDElementGetUsage(element) == usage
        };
        if !matches {
            continue;
        }
        match current_value(device, element) {
            Some(0) => state = state.or(Some(ButtonState::Released)),
            Some(_) => return Some(ButtonState::Pressed),
            None => state = Some(ButtonState::Unknown),
        }
    }
    state
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

/// The one Logitech device holding the button, provided every other
/// potential source is confirmed released.
fn unique_pressed_logitech(candidates: &[ButtonCandidate]) -> Option<EventDevice> {
    if candidates
        .iter()
        .any(|candidate| candidate.state == ButtonState::Unknown)
    {
        return None;
    }
    let mut pressed = candidates
        .iter()
        .filter(|candidate| candidate.state == ButtonState::Pressed);
    let source = pressed.next()?;
    if pressed.next().is_some() || !source.device.is_logitech() {
        return None;
    }
    Some(source.device.clone())
}

#[cfg(test)]
mod tests;
