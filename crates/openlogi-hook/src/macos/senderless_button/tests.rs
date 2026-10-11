use super::*;

const BACK: i64 = 3;

fn device(vendor_id: u32) -> EventDevice {
    EventDevice {
        vendor_id: Some(vendor_id),
        ..EventDevice::default()
    }
}

fn candidate(vendor_id: u32, state: ButtonState) -> ButtonCandidate {
    ButtonCandidate {
        device: device(vendor_id),
        state,
    }
}

fn logitech() -> EventDevice {
    device(0x046d)
}

/// A resolver holding a sender-less Logitech attribution for Back.
fn holding_senderless_back() -> SenderlessButtonResolver {
    let mut resolver = SenderlessButtonResolver::unavailable();
    let only_logitech = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        resolver.resolve_press(BACK, Some(&only_logitech)),
        Some(logitech())
    );
    resolver
}

#[test]
fn selects_only_one_pressed_logitech_device() {
    let values = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Released),
    ];
    assert_eq!(
        unique_pressed_logitech(&values),
        Some(values[0].device.clone())
    );
}

#[test]
fn rejects_non_logitech_ambiguous_and_unpressed_sources() {
    use ButtonState::{Pressed, Released};
    assert!(unique_pressed_logitech(&[candidate(0x4f53, Pressed)]).is_none());
    assert!(
        unique_pressed_logitech(&[candidate(0x046d, Pressed), candidate(0x4f53, Pressed)])
            .is_none()
    );
    assert!(unique_pressed_logitech(&[candidate(0x046d, Released)]).is_none());
}

#[test]
fn an_unreadable_mouse_blocks_attribution() {
    // A competing mouse whose button state could not be read may be the one
    // pressing; the readable Logitech must not look unique because of it.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let unknown_rival = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Unknown),
    ];
    assert!(resolver.resolve_press(BACK, Some(&unknown_rival)).is_none());
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none(),
        "a refused press must not leave an attribution for its release"
    );
}

#[test]
fn a_missing_hid_manager_never_attributes() {
    let mut resolver = SenderlessButtonResolver::unavailable();
    assert!(
        resolver
            .resolve(BACK, true, ButtonSender::Unidentified)
            .is_none()
    );
}

#[test]
fn sender_ids_classify_posted_senderless_and_attributed_events() {
    let lookup = |_| panic!("only a nonzero sender id names a device");
    assert!(matches!(
        ButtonSender::from_sender_id(None, lookup),
        ButtonSender::Posted
    ));
    assert!(matches!(
        ButtonSender::from_sender_id(Some(0), lookup),
        ButtonSender::Unidentified
    ));
    assert!(matches!(
        ButtonSender::from_sender_id(Some(7), |id| {
            assert_eq!(id, 7);
            logitech()
        }),
        ButtonSender::Device(device) if device == logitech()
    ));
}

#[test]
fn posted_events_neither_borrow_nor_consume_a_held_attribution() {
    let mut resolver = holding_senderless_back();

    assert!(resolver.resolve(BACK, true, ButtonSender::Posted).is_none());
    assert!(!resolver.take_attribution_invalidated());
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Posted)
            .is_none()
    );

    assert_eq!(
        resolver.resolve(BACK, false, ButtonSender::Unidentified),
        Some(logitech()),
        "the physical release must still find its press-time identity"
    );
}

#[test]
fn release_uses_the_source_cached_on_press() {
    let mut resolver = holding_senderless_back();

    assert_eq!(
        resolver.resolve(BACK, false, ButtonSender::Unidentified),
        Some(logitech())
    );
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none()
    );
}

#[test]
fn an_ambiguous_press_invalidates_a_stale_cached_attribution() {
    let mut resolver = holding_senderless_back();
    let ambiguous = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Pressed),
    ];

    assert!(resolver.resolve_press(BACK, Some(&ambiguous)).is_none());
    assert!(resolver.take_attribution_invalidated());
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none(),
        "the stale cache entry must not survive the ambiguous press, or the \
         second mouse's release would be misattributed to the first"
    );
}

#[test]
fn a_press_of_a_held_button_is_never_attributed_to_its_holder() {
    // A device cannot press a button it still holds, so a second sender-less
    // press — even one the HID poll cannot see the source of — must pass
    // through rather than borrow the holder's identity.
    let mut resolver = holding_senderless_back();
    let only_holder = [candidate(0x046d, ButtonState::Pressed)];

    assert!(resolver.resolve_press(BACK, Some(&only_holder)).is_none());
    assert!(resolver.take_attribution_invalidated());

    let mut resolver = SenderlessButtonResolver::unavailable();
    assert_eq!(
        resolver.resolve(BACK, true, ButtonSender::Device(logitech())),
        Some(logitech())
    );
    assert!(resolver.resolve_press(BACK, Some(&only_holder)).is_none());
    assert!(
        !resolver.take_attribution_invalidated(),
        "an attributed hold still gets its attributed release"
    );
}

#[test]
fn another_devices_attributed_release_keeps_a_senderless_attribution() {
    let mut resolver = holding_senderless_back();
    let other = device(0x4f53);

    assert_eq!(
        resolver.resolve(BACK, false, ButtonSender::Device(other.clone())),
        Some(other)
    );
    assert_eq!(
        resolver.resolve(BACK, false, ButtonSender::Unidentified),
        Some(logitech())
    );
}

#[test]
fn cancel_all_drops_every_cached_attribution() {
    let mut resolver = holding_senderless_back();
    assert_eq!(
        resolver.resolve(BACK + 1, true, ButtonSender::Device(logitech())),
        Some(logitech())
    );

    resolver.cancel_all();

    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none()
    );
    assert!(
        resolver
            .resolve(BACK + 1, false, ButtonSender::Unidentified)
            .is_none()
    );
}
