//! Serial controller port state.

/// A latched controller. Button bits use the SNES serial order from low to
/// high: B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerState {
    pub buttons: u16,
    strobe: bool,
    shift: u16,
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            buttons: 0,
            strobe: false,
            shift: 0,
        }
    }
}

impl ControllerState {
    pub fn set_buttons(&mut self, buttons: u16) {
        self.buttons = buttons;
        if self.strobe {
            self.shift = buttons;
        }
    }

    pub fn write_strobe(&mut self, value: u8) {
        let strobe = value & 1 != 0;
        if strobe || self.strobe {
            self.shift = self.buttons;
        }
        self.strobe = strobe;
    }

    pub fn read_serial(&mut self) -> u8 {
        let value = if self.strobe { self.buttons & 1 } else { self.shift & 1 };
        if !self.strobe {
            self.shift = (self.shift >> 1) | 0x8000;
        }
        value as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_latches_and_shifts_buttons() {
        let mut controller = ControllerState::default();
        controller.set_buttons(0b101);
        controller.write_strobe(1);
        controller.write_strobe(0);
        assert_eq!(controller.read_serial(), 1);
        assert_eq!(controller.read_serial(), 0);
        assert_eq!(controller.read_serial(), 1);
    }
}
