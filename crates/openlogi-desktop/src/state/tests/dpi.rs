//! `dpi_for_current`'s fallback order before the live capability read resolves.

use super::*;
use crate::state::DEFAULT_DPI;

#[test]
fn the_persisted_config_value_is_shown_before_the_live_read_resolves() {
    let mut state = state_with_a_known_mouse();
    state
        .config
        .edit(|config| config.set_dpi(KNOWN_MOUSE_KEY, Dpi::new(400)));

    assert_eq!(
        state.dpi_for_current(),
        Dpi::new(400),
        "the value the user configured must be shown while the agent's live \
         DPI read is still pending, not an unrelated hardcoded default"
    );
}

#[test]
fn the_hardcoded_default_is_shown_with_neither_a_live_read_nor_a_configured_value() {
    let state = state_with_a_known_mouse();

    assert_eq!(state.dpi_for_current(), DEFAULT_DPI);
}
