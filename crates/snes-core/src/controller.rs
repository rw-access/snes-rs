//! Controller state boundary.

/// Placeholder for a connected controller's latched state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControllerState {
    pub buttons: u16,
}
