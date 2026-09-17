use super::*;

fn candidate(vendor_id: u32, pressed: bool) -> ButtonCandidate {
    ButtonCandidate {
        device: EventDevice {
            vendor_id: Some(vendor_id),
            ..EventDevice::default()
        },
        pressed,
    }
}

#[test]
fn selects_only_one_pressed_logitech_device() {
    let values = [candidate(0x046d, true), candidate(0x4f53, false)];
    assert_eq!(
        unique_pressed_logitech(&values),
        Some(values[0].device.clone())
    );
}

#[test]
fn rejects_non_logitech_ambiguous_and_unpressed_sources() {
    assert!(unique_pressed_logitech(&[candidate(0x4f53, true)]).is_none());
    assert!(unique_pressed_logitech(&[candidate(0x046d, true), candidate(0x4f53, true)]).is_none());
    assert!(unique_pressed_logitech(&[candidate(0x046d, false)]).is_none());
}

#[test]
fn release_uses_the_source_cached_on_press() {
    let mut resolver = SenderlessButtonResolver::unavailable();
    resolver
        .held_sources
        .insert(4, candidate(0x046d, true).device);

    let released = resolver.resolve(4, false, None);

    assert!(released.as_ref().is_some_and(EventDevice::is_logitech));
    assert!(resolver.resolve(4, false, None).is_none());
}
