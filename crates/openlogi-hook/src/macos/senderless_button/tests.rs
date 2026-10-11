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

fn identified(sender_id: u64, device: EventDevice) -> ButtonSender {
    ButtonSender::Device { sender_id, device }
}

/// One transition with `poll` standing in for the `IOHIDManager` read.
fn step(
    resolver: &mut SenderlessButtonResolver,
    button_number: i64,
    pressed: bool,
    sender: ButtonSender,
    poll: Option<&[ButtonCandidate]>,
) -> Option<EventDevice> {
    resolver
        .tracker
        .transition(button_number, pressed, sender, || poll.map(<[_]>::to_vec))
}

fn press(
    resolver: &mut SenderlessButtonResolver,
    button_number: i64,
    poll: Option<&[ButtonCandidate]>,
) -> Option<EventDevice> {
    step(
        resolver,
        button_number,
        true,
        ButtonSender::Unidentified,
        poll,
    )
}

/// A resolver holding a sender-less Logitech attribution for Back.
fn holding_senderless_back() -> SenderlessButtonResolver {
    let mut resolver = SenderlessButtonResolver::unavailable();
    let only_logitech = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        press(&mut resolver, BACK, Some(&only_logitech)),
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
    assert!(press(&mut resolver, BACK, Some(&unknown_rival)).is_none());
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
        ButtonSender::Device { sender_id: 7, device } if device == logitech()
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
fn the_holders_state_decides_whose_senderless_release_it_is() {
    let ambiguous = [
        candidate(0x046d, ButtonState::Pressed),
        candidate(0x4f53, ButtonState::Pressed),
    ];
    let holder_pressed = [candidate(0x046d, ButtonState::Pressed)];
    let holder_released = [candidate(0x046d, ButtonState::Released)];
    let holder_unknown = [candidate(0x046d, ButtonState::Unknown)];

    let mut resolver = holding_senderless_back();
    assert!(press(&mut resolver, BACK, Some(&ambiguous)).is_none());
    assert!(!resolver.take_attribution_invalidated());
    let release =
        |resolver: &mut _, poll| step(resolver, BACK, false, ButtonSender::Unidentified, poll);
    assert!(
        release(&mut resolver, Some(&holder_pressed)).is_none(),
        "the second mouse's release must not end the held attribution"
    );
    assert_eq!(
        release(&mut resolver, Some(&holder_released)),
        Some(logitech())
    );

    let mut resolver = holding_senderless_back();
    assert!(press(&mut resolver, BACK, Some(&ambiguous)).is_none());
    assert_eq!(
        release(&mut resolver, Some(&holder_released)),
        Some(logitech())
    );
    assert!(release(&mut resolver, Some(&holder_released)).is_none());

    let mut resolver = holding_senderless_back();
    assert!(press(&mut resolver, BACK, Some(&ambiguous)).is_none());
    assert!(release(&mut resolver, Some(&holder_unknown)).is_none());
    assert!(resolver.take_attribution_invalidated());
    assert!(release(&mut resolver, Some(&holder_unknown)).is_none());
    assert!(resolver.tracker.holds.is_empty());
}

#[test]
fn a_press_of_a_held_button_is_never_attributed_to_its_holder() {
    // A device cannot press a button it still holds, so a second sender-less
    // press — even one the HID poll cannot see the source of — must pass
    // through rather than borrow the holder's identity.
    let mut resolver = holding_senderless_back();
    let only_holder = [candidate(0x046d, ButtonState::Pressed)];

    assert!(press(&mut resolver, BACK, Some(&only_holder)).is_none());
    assert!(!resolver.take_attribution_invalidated());
}

#[test]
fn an_identified_hold_is_never_borrowed_by_a_senderless_press() {
    // An identified Logitech holds Back; a second mouse's sender-less click
    // has already released by poll time, so only the holder reads pressed.
    let mut resolver = SenderlessButtonResolver::unavailable();
    let only_holder = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        step(&mut resolver, BACK, true, identified(7, logitech()), None),
        Some(logitech())
    );

    assert!(press(&mut resolver, BACK, Some(&only_holder)).is_none());
    assert!(
        !resolver.take_attribution_invalidated(),
        "an attributed hold still gets its attributed release"
    );
    assert!(
        step(
            &mut resolver,
            BACK,
            false,
            ButtonSender::Unidentified,
            Some(&only_holder)
        )
        .is_none()
    );
    assert_eq!(
        step(&mut resolver, BACK, false, identified(7, logitech()), None),
        Some(logitech())
    );
    assert!(resolver.tracker.holds.is_empty());
}

#[test]
fn an_identified_press_keeps_a_held_senderless_attribution() {
    // Hold Back on a sender-less Logitech mouse, then press and release Back
    // on an identified mouse: the Logitech release must still find its
    // press-time identity, or the hold begun under it would never end.
    let mut resolver = holding_senderless_back();
    let other = device(0x4f53);

    assert_eq!(
        resolver.resolve(BACK, true, identified(9, other.clone())),
        Some(other.clone())
    );
    assert!(!resolver.take_attribution_invalidated());
    assert_eq!(
        resolver.resolve(BACK, false, identified(9, other.clone())),
        Some(other)
    );
    assert_eq!(
        resolver.resolve(BACK, false, ButtonSender::Unidentified),
        Some(logitech())
    );
}

#[test]
fn the_holders_identified_release_clears_its_senderless_attribution() {
    let mut resolver = holding_senderless_back();

    assert_eq!(
        resolver.resolve(BACK, false, identified(7, logitech())),
        Some(logitech())
    );
    assert!(
        resolver
            .resolve(BACK, false, ButtonSender::Unidentified)
            .is_none()
    );
}

#[test]
fn cancel_all_drops_every_cached_attribution() {
    let mut resolver = holding_senderless_back();
    let only_logitech = [candidate(0x046d, ButtonState::Pressed)];
    assert_eq!(
        press(&mut resolver, BACK + 1, Some(&only_logitech)),
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

/// The physical sources of one Back press each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actor {
    IdentifiedLogitech,
    SenderlessLogitech,
    SecondMouse,
    Posted,
}

const ACTORS: [Actor; 4] = [
    Actor::IdentifiedLogitech,
    Actor::SenderlessLogitech,
    Actor::SecondMouse,
    Actor::Posted,
];

/// What the `IOHIDManager` poll reports for one physical mouse.
#[derive(Clone, Copy, Debug)]
enum PollView {
    Truthful,
    /// A click too fast for the poll, or a mouse it cannot see pressed.
    AlwaysReleased,
    Unknown,
    /// Truthful on the first read, unreadable after.
    FailsLater,
}

const VIEWS: [PollView; 4] = [
    PollView::Truthful,
    PollView::AlwaysReleased,
    PollView::Unknown,
    PollView::FailsLater,
];

/// The three HID mice, indexed like the first three `ACTORS`.
fn mouse(index: usize) -> EventDevice {
    EventDevice {
        vendor_id: Some(if index == 2 { 0x4f53 } else { 0x046d }),
        product_id: Some(u32::try_from(index).unwrap()),
        product_name: None,
    }
}

fn interleavings(
    prefix: &mut Vec<(usize, bool)>,
    progress: &mut [u8; 4],
    out: &mut Vec<Vec<(usize, bool)>>,
) {
    if progress.iter().all(|&done| done == 2) {
        out.push(prefix.clone());
        return;
    }
    for actor in 0..ACTORS.len() {
        if progress[actor] == 2 {
            continue;
        }
        let pressed = progress[actor] == 0;
        progress[actor] += 1;
        prefix.push((actor, pressed));
        interleavings(prefix, progress, out);
        prefix.pop();
        progress[actor] -= 1;
    }
}

/// Every ordering of one press and release per actor, under every poll view
/// per mouse and both release senders for the sender-less Logitech, checked
/// against the invariants a remapping runtime relies on.
#[test]
fn every_interleaving_attributes_only_physical_presses_and_closes_every_hold() {
    let mut orders = Vec::new();
    interleavings(&mut Vec::new(), &mut [0; 4], &mut orders);
    assert_eq!(orders.len(), 2520);

    for order in &orders {
        for views in VIEWS
            .iter()
            .flat_map(|&a| VIEWS.iter().flat_map(move |&b| VIEWS.map(|c| [a, b, c])))
        {
            for identified_release in [false, true] {
                check_interleaving(order, views, identified_release);
            }
        }
    }
}

fn check_interleaving(order: &[(usize, bool)], views: [PollView; 3], identified_release: bool) {
    let context = || format!("{order:?} {views:?} identified release: {identified_release}");
    let mut resolver = SenderlessButtonResolver::unavailable();
    let mut down = [false; 3];
    let mut polled_before = false;
    // The runtime's view: whether a hold begun under a sender-less
    // attribution is still open.
    let mut hold_open = false;

    for &(actor, pressed) in order {
        if actor < 3 {
            down[actor] = pressed;
        }
        let sender = match ACTORS[actor] {
            Actor::IdentifiedLogitech => identified(7, mouse(0)),
            Actor::SenderlessLogitech if !pressed && identified_release => identified(9, mouse(1)),
            Actor::SenderlessLogitech | Actor::SecondMouse => ButtonSender::Unidentified,
            Actor::Posted => ButtonSender::Posted,
        };
        let senderless = matches!(sender, ButtonSender::Unidentified);
        let poll: Vec<_> = (0..3)
            .map(|index| {
                let truth = if down[index] {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                let state = match views[index] {
                    PollView::Truthful => truth,
                    PollView::FailsLater if !polled_before => truth,
                    PollView::AlwaysReleased => ButtonState::Released,
                    PollView::Unknown | PollView::FailsLater => ButtonState::Unknown,
                };
                ButtonCandidate {
                    device: mouse(index),
                    state,
                }
            })
            .collect();
        let result = resolver.tracker.transition(BACK, pressed, sender, || {
            polled_before = true;
            Some(poll)
        });
        let invalidated = resolver.take_attribution_invalidated();

        match ACTORS[actor] {
            Actor::Posted => assert!(result.is_none(), "{}", context()),
            Actor::IdentifiedLogitech => assert_eq!(result, Some(mouse(0)), "{}", context()),
            Actor::SenderlessLogitech | Actor::SecondMouse if !senderless => {
                assert_eq!(result, Some(mouse(actor)), "{}", context());
            }
            Actor::SenderlessLogitech | Actor::SecondMouse => {
                if let Some(source) = &result {
                    assert!(
                        ACTORS[actor] == Actor::SenderlessLogitech && *source == mouse(1),
                        "attributed to a mouse that is not this event's source: {}",
                        context()
                    );
                }
            }
        }

        let attributed_senderless = result.as_ref() == Some(&mouse(1));
        if pressed && attributed_senderless {
            assert!(!hold_open, "{}", context());
            hold_open = true;
        } else if !pressed && attributed_senderless {
            assert!(
                hold_open || !senderless,
                "a sender-less release found an attribution no press made: {}",
                context()
            );
            hold_open = false;
        }
        if invalidated {
            assert!(
                hold_open && matches!(views[1], PollView::FailsLater),
                "invalidated without an unreadable holder: {}",
                context()
            );
            hold_open = false;
        }
        if ACTORS[actor] == Actor::SenderlessLogitech && !pressed {
            assert!(
                !hold_open,
                "hold outlived its owner's release: {}",
                context()
            );
        }
    }
    assert!(
        resolver.tracker.holds.is_empty(),
        "state leaked: {}",
        context()
    );
}
