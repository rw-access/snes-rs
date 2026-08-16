//! Serial controller port state.

/// The sixteen-bit report emitted by a standard SNES controller.
///
/// Bits are delivered least-significant bit first: B, Y, Select, Start, Up,
/// Down, Left, Right, A, X, L, R, followed by four zero signature bits. Once
/// the report is exhausted, an attached standard controller returns ones.
pub const STANDARD_CONTROLLER_BUTTON_MASK: u16 = 0x0fff;

/// The two CPU-visible serial controller inputs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerPort {
    /// JOYSER0, CPU address `$4016`.
    Port1,
    /// JOYSER1, CPU address `$4017`.
    Port2,
}

/// A latched controller. Button bits use the SNES serial order from low to
/// high: B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerState {
    pub buttons: u16,
    strobe: bool,
    shift: u16,
    connected: bool,
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            buttons: 0,
            strobe: false,
            shift: 0,
            connected: true,
        }
    }
}

impl ControllerState {
    pub fn set_buttons(&mut self, buttons: u16) {
        self.buttons = buttons;
        if self.strobe {
            self.shift = self.report();
        }
    }

    /// Mark whether a physical device is attached to this port.
    ///
    /// A disconnected SNES controller drives zeroes, including after its
    /// sixteen-bit report has been consumed. The default remains connected so
    /// the existing zero-button controller behavior is preserved.
    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
        if self.strobe {
            self.shift = self.report();
        }
    }

    pub fn write_strobe(&mut self, value: u8) {
        let strobe = value & 1 != 0;
        if strobe || self.strobe {
            self.shift = self.report();
        }
        self.strobe = strobe;
    }

    /// Read the next serial bit from this controller.
    ///
    /// While strobe is high the first (B) bit is presented continuously and
    /// reads do not advance the shift register. A low strobe clocks one bit
    /// and then shifts in the attached-controller tail value.
    pub fn read_serial(&mut self) -> u8 {
        let value = if self.strobe {
            self.first_bit()
        } else {
            (self.shift & 1) as u8
        };
        if !self.strobe {
            self.shift = (self.shift >> 1) | u16::from(self.tail_bit()) << 15;
        }
        value as u8
    }

    /// Read JOYSER0 while preserving the caller's open-bus bits.
    pub fn read_serial_with_open_bus(&mut self, open_bus: u8) -> u8 {
        // A standard pad has no D1 wire; the console pulls that input low.
        (open_bus & 0xfc) | self.read_serial()
    }

    fn report(&self) -> u16 {
        if self.connected {
            self.buttons & STANDARD_CONTROLLER_BUTTON_MASK
        } else {
            0
        }
    }

    fn first_bit(&self) -> u8 {
        if self.connected {
            (self.buttons & 1) as u8
        } else {
            0
        }
    }

    fn tail_bit(&self) -> u8 {
        u8::from(self.connected)
    }
}

/// The pair of standard SNES controller ports.
///
/// This type is intentionally separate from [`ControllerState`]. The
/// existing console API owns one `ControllerState` and remains source
/// compatible, while new integrations can model both `$4016` and `$4017`
/// without duplicating the shared strobe behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerPorts {
    pub port1: ControllerState,
    pub port2: ControllerState,
    auto_read: [u16; 2],
}

impl Default for ControllerPorts {
    fn default() -> Self {
        Self {
            port1: ControllerState::default(),
            port2: ControllerState::default(),
            auto_read: [0; 2],
        }
    }
}

impl ControllerPorts {
    /// Set the shared JOYOUT strobe for both controller ports.
    pub fn write_strobe(&mut self, value: u8) {
        self.port1.write_strobe(value);
        self.port2.write_strobe(value);
    }

    /// Read one of the two serial inputs, retaining the hardware's fixed and
    /// open-bus bits. The supplied `open_bus` is used only for bits that the
    /// selected serial input does not drive.
    pub fn read_port(&mut self, port: ControllerPort, open_bus: u8) -> u8 {
        match port {
            ControllerPort::Port1 => self.port1.read_serial_with_open_bus(open_bus),
            ControllerPort::Port2 => {
                // JOYSER1 drives D4-D2 high; D1-D0 are the peripheral data
                // lines. A standard controller only drives D0, so D1 stays
                // low and the upper three bits retain the open bus.
                (open_bus & 0xe0) | 0x1c | self.port2.read_serial()
            }
        }
    }

    pub fn read_port1(&mut self, open_bus: u8) -> u8 {
        self.read_port(ControllerPort::Port1, open_bus)
    }

    pub fn read_port2(&mut self, open_bus: u8) -> u8 {
        self.read_port(ControllerPort::Port2, open_bus)
    }

    /// Captures the two standard-controller reports exposed by the SNES
    /// auto-joypad read registers during vblank.
    pub fn latch_auto_read(&mut self) {
        self.auto_read = [self.port1.report(), self.port2.report()];
    }

    /// Reads one byte of the auto-joypad register block ($4218-$421b).
    pub fn read_auto_register(&self, address: u16) -> u8 {
        match address {
            0x4218 => self.auto_read[0] as u8,
            0x4219 => (self.auto_read[0] >> 8) as u8,
            0x421a => self.auto_read[1] as u8,
            0x421b => (self.auto_read[1] >> 8) as u8,
            _ => 0,
        }
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

    #[test]
    fn standard_report_has_signature_zeroes_then_ones() {
        let mut controller = ControllerState::default();
        controller.set_buttons(0x0a55);
        controller.write_strobe(1);
        controller.write_strobe(0);

        let mut report = 0u16;
        for bit in 0..16 {
            report |= u16::from(controller.read_serial()) << bit;
        }
        assert_eq!(report, 0x0a55);
        assert_eq!(controller.read_serial(), 1);
        assert_eq!(controller.read_serial(), 1);
    }

    #[test]
    fn strobe_high_repeats_first_bit_and_falling_edge_latches() {
        let mut controller = ControllerState::default();
        controller.set_buttons(0b01);
        controller.write_strobe(1);
        assert_eq!(controller.read_serial(), 1);
        assert_eq!(controller.read_serial(), 1);

        controller.set_buttons(0);
        assert_eq!(controller.read_serial(), 0);
        controller.write_strobe(0);
        assert_eq!(controller.read_serial(), 0);
        assert_eq!(controller.read_serial(), 0);
    }

    #[test]
    fn disconnected_controller_drives_zeroes_after_report() {
        let mut controller = ControllerState::default();
        controller.set_connected(false);
        controller.set_buttons(u16::MAX);
        controller.write_strobe(0);
        for _ in 0..20 {
            assert_eq!(controller.read_serial(), 0);
        }
    }

    #[test]
    fn two_ports_share_strobe_but_shift_independently() {
        let mut ports = ControllerPorts::default();
        ports.port1.set_buttons(1 << 0);
        ports.port2.set_buttons(1 << 1);
        ports.write_strobe(1);
        ports.write_strobe(0);

        assert_eq!(ports.read_port1(0), 0x01);
        assert_eq!(ports.read_port2(0), 0x1c);
        assert_eq!(ports.read_port1(0), 0x00);
        assert_eq!(ports.read_port2(0), 0x1d);
    }

    #[test]
    fn serial_reads_preserve_open_bus_bits() {
        let mut ports = ControllerPorts::default();
        ports.write_strobe(1);

        assert_eq!(ports.read_port1(0xa7), 0xa4);
        assert_eq!(ports.read_port2(0xa7), 0xbc);
    }

    #[test]
    fn button_bits_above_standard_report_are_not_serialized() {
        let mut controller = ControllerState::default();
        controller.set_buttons(0xf000);
        controller.write_strobe(1);
        controller.write_strobe(0);
        for _ in 0..16 {
            assert_eq!(controller.read_serial(), 0);
        }
        assert_eq!(controller.read_serial(), 1);
    }

    #[test]
    fn auto_read_registers_capture_both_controller_reports() {
        let mut ports = ControllerPorts::default();
        ports.port1.set_buttons(0x0123);
        ports.port2.set_buttons(0x0456);
        ports.latch_auto_read();

        assert_eq!(ports.read_auto_register(0x4218), 0x23);
        assert_eq!(ports.read_auto_register(0x4219), 0x01);
        assert_eq!(ports.read_auto_register(0x421a), 0x56);
        assert_eq!(ports.read_auto_register(0x421b), 0x04);
    }
}
