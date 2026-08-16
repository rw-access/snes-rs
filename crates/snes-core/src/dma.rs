//! General DMA and bounded per-scanline HDMA scheduling.

use crate::{bus::Address24, cpu65816::CpuBus};

/// One of the eight general-DMA channels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DmaChannel {
    pub control: u8,
    pub destination: u8,
    pub source_address: u16,
    pub source_bank: u8,
    pub size: u16,
}

/// Runtime state for one HDMA channel.
///
/// `table_address` corresponds to the hardware A2A register. The initial
/// value is copied from [`DmaChannel::source_address`] when a frame starts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HdmaChannelState {
    pub table_address: u16,
    pub indirect_address: u16,
    /// The bank used by indirect HDMA data accesses (DASBn).
    pub indirect_bank: u8,
    pub line_counter: u8,
    pub repeat: bool,
    pub initialized: bool,
    pub terminated: bool,
    pub transfer_pending: bool,
}

/// General DMA controller state and bounded per-scanline HDMA scheduler.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Dma {
    pub channels: [DmaChannel; 8],
    pub hdma_channels: [HdmaChannelState; 8],
    pub hdma_enable: u8,
    pub pending: u8,
}

impl Dma {
    pub fn write_register(&mut self, address: u16, value: u8) {
        if (0x4300..=0x437f).contains(&address) {
            let channel = usize::from((address - 0x4300) / 0x10);
            let register = (address - 0x4300) % 0x10;
            let state = &mut self.channels[channel];
            match register {
                0x0 => state.control = value,
                0x1 => state.destination = value,
                0x2 => state.source_address = (state.source_address & 0xff00) | u16::from(value),
                0x3 => {
                    state.source_address = (state.source_address & 0x00ff) | (u16::from(value) << 8)
                }
                0x4 => state.source_bank = value,
                0x5 => state.size = (state.size & 0xff00) | u16::from(value),
                0x6 => state.size = (state.size & 0x00ff) | (u16::from(value) << 8),
                0x7 => {}
                _ => {}
            }
            self.write_hdma_runtime_register(channel, register, value);
        } else if address == 0x420c {
            self.hdma_enable = value;
            // The hardware samples the enabled channels at the start of
            // scanline zero. Invalidate runtime state so the next explicit
            // frame start (or lazy first scanline) re-reads table starts.
            for channel in &mut self.hdma_channels {
                let indirect_bank = channel.indirect_bank;
                *channel = HdmaChannelState::default();
                channel.indirect_bank = indirect_bank;
            }
        } else if address == 0x420b {
            // MDMAEN is a write-one start mask, not a persistent OR latch.
            // The console consumes this value after the current instruction.
            self.pending = value;
        }
    }

    /// Reads the implemented DMA/HDMA channel registers.
    ///
    /// The ordinary DMA count remains visible through DASnL/DASnH. Once HDMA
    /// has started, A2AnL/A2AnH and NLTRn expose the corresponding runtime
    /// table position and line descriptor state.
    pub fn read_register(&self, address: u16) -> u8 {
        if address == 0x420c {
            return self.hdma_enable;
        }
        if !(0x4300..=0x437f).contains(&address) {
            return 0;
        }
        let channel = usize::from((address - 0x4300) / 0x10);
        let register = (address - 0x4300) % 0x10;
        let state = &self.channels[channel];
        let hdma = &self.hdma_channels[channel];
        match register {
            0x0 => state.control,
            0x1 => state.destination,
            0x2 => state.source_address as u8,
            0x3 => (state.source_address >> 8) as u8,
            0x4 => state.source_bank,
            0x5 => state.size as u8,
            0x6 => (state.size >> 8) as u8,
            0x7 => hdma.indirect_bank,
            0x8 => {
                if hdma.initialized {
                    hdma.table_address as u8
                } else {
                    state.source_address as u8
                }
            }
            0x9 => {
                if hdma.initialized {
                    (hdma.table_address >> 8) as u8
                } else {
                    (state.source_address >> 8) as u8
                }
            }
            0xa => {
                if hdma.initialized {
                    hdma.line_counter | if hdma.repeat { 0x80 } else { 0 }
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    fn write_hdma_runtime_register(&mut self, channel: usize, register: u16, value: u8) {
        let state = &mut self.hdma_channels[channel];
        match register {
            0x7 => state.indirect_bank = value,
            // A2A and NLTR are writable runtime registers on the hardware.
            // Preserve that useful behavior for tests and debugger tooling.
            0x8 => state.table_address = (state.table_address & 0xff00) | u16::from(value),
            0x9 => state.table_address = (state.table_address & 0x00ff) | (u16::from(value) << 8),
            0xa => {
                state.line_counter = value & 0x7f;
                state.repeat = value & 0x80 != 0;
                state.transfer_pending = state.line_counter != 0;
                state.terminated = false;
            }
            _ => {}
        }
    }

    pub fn take_pending(&mut self) -> u8 {
        let pending = self.pending;
        self.pending = 0;
        pending
    }

    /// Executes pending channels and returns the number of bus transfers.
    pub fn execute(&mut self, mask: u8, bus: &mut dyn CpuBus) -> u32 {
        let mut transfers = 0;
        for index in 0..8 {
            if mask & (1 << index) == 0 {
                continue;
            }
            transfers += self.execute_channel(index, bus);
        }
        transfers
    }

    fn execute_channel(&mut self, index: usize, bus: &mut dyn CpuBus) -> u32 {
        let channel = self.channels[index];
        let mut source_address = channel.source_address;
        let count = if channel.size == 0 {
            0x1_0000
        } else {
            u32::from(channel.size)
        };
        let direction_from_ppu = channel.control & 0x80 != 0;
        let fixed_source = channel.control & 0x08 != 0;
        let decrement_source = channel.control & 0x10 != 0;
        let (pattern, pattern_len) = transfer_pattern(channel.control);
        let mut transfers = 0;
        for offset in 0..count {
            let ppu_address = Address24::new(
                0x002100
                    + u32::from(
                        channel
                            .destination
                            .wrapping_add(pattern[(offset as usize) % pattern_len]),
                    ),
            );
            let source = hdma_address(channel.source_bank, source_address);
            if direction_from_ppu {
                let value = bus.read(ppu_address);
                bus.write(source, value);
            } else {
                let value = bus.read(source);
                bus.write(ppu_address, value);
            }
            transfers += 1;
            if !fixed_source {
                source_address = if decrement_source {
                    source_address.wrapping_sub(1)
                } else {
                    source_address.wrapping_add(1)
                };
            }
        }
        // A1T wraps within the configured A-bus bank; A1B is not incremented
        // when a transfer crosses $ffff.
        self.channels[index].source_address = source_address;
        self.channels[index].size = 0;
        transfers
    }

    /// Starts a new HDMA frame using the channels selected by HDMAEN.
    ///
    /// Call this at the beginning of scanline zero. The scheduler is also
    /// lazy: calling [`Self::run_hdma_scanline`] without this method first
    /// initializes any enabled channel, which keeps standalone tests simple.
    pub fn begin_hdma_frame(&mut self) {
        for index in 0..8 {
            if self.hdma_enable & (1 << index) == 0 {
                self.hdma_channels[index] = HdmaChannelState::default();
                continue;
            }
            self.initialize_hdma_channel(index);
        }
    }

    /// Executes one bounded HDMA scheduling point, normally at the end of a
    /// scanline/HBlank. Returns the number of A/B bus byte transfers issued.
    ///
    /// This models table and line scheduling, but leaves exact HBlank
    /// arbitration and master-clock costs to the console timing layer.
    pub fn run_hdma_scanline(&mut self, bus: &mut dyn CpuBus) -> u32 {
        let mut transfers = 0;
        for index in 0..8 {
            if self.hdma_enable & (1 << index) == 0 {
                continue;
            }
            if !self.hdma_channels[index].initialized {
                self.initialize_hdma_channel(index);
            }
            transfers += self.execute_hdma_channel(index, bus);
        }
        transfers
    }

    /// Returns channels whose HDMA tables have not reached a zero descriptor.
    pub fn active_hdma_mask(&self) -> u8 {
        self.hdma_enable
            & self
                .hdma_channels
                .iter()
                .enumerate()
                .fold(0u8, |mask, (index, state)| {
                    mask | if state.initialized && !state.terminated {
                        1u8 << index
                    } else {
                        0
                    }
                })
    }

    fn initialize_hdma_channel(&mut self, index: usize) {
        let channel = self.channels[index];
        let indirect_bank = self.hdma_channels[index].indirect_bank;
        self.hdma_channels[index] = HdmaChannelState {
            table_address: channel.source_address,
            indirect_address: channel.size,
            indirect_bank,
            line_counter: 0,
            repeat: false,
            initialized: true,
            terminated: false,
            transfer_pending: false,
        };
    }

    fn execute_hdma_channel(&mut self, index: usize, bus: &mut dyn CpuBus) -> u32 {
        if self.hdma_channels[index].terminated {
            return 0;
        }

        if self.hdma_channels[index].line_counter == 0 && !self.load_hdma_descriptor(index, bus) {
            return 0;
        }

        let transfers = if self.hdma_channels[index].transfer_pending {
            self.transfer_hdma_pattern(index, bus)
        } else {
            0
        };

        let state = &mut self.hdma_channels[index];
        state.line_counter = state.line_counter.saturating_sub(1);
        if state.line_counter == 0 {
            state.transfer_pending = false;
        } else {
            // Non-repeat entries write once, then consume the remaining
            // scanlines without consuming table/indirect data.
            state.transfer_pending = state.repeat;
        }
        transfers
    }

    fn load_hdma_descriptor(&mut self, index: usize, bus: &mut dyn CpuBus) -> bool {
        let channel = self.channels[index];
        let state = &mut self.hdma_channels[index];
        let descriptor = bus.read(hdma_address(channel.source_bank, state.table_address));
        state.table_address = state.table_address.wrapping_add(1);
        state.line_counter = descriptor & 0x7f;
        state.repeat = descriptor & 0x80 != 0;
        state.transfer_pending = state.line_counter != 0;
        if state.line_counter == 0 {
            state.terminated = true;
            state.transfer_pending = false;
            return false;
        }

        if channel.control & 0x40 != 0 {
            let low = bus.read(hdma_address(channel.source_bank, state.table_address));
            state.table_address = state.table_address.wrapping_add(1);
            let high = bus.read(hdma_address(channel.source_bank, state.table_address));
            state.table_address = state.table_address.wrapping_add(1);
            state.indirect_address = u16::from_le_bytes([low, high]);
        }
        true
    }

    fn transfer_hdma_pattern(&mut self, index: usize, bus: &mut dyn CpuBus) -> u32 {
        let channel = self.channels[index];
        let state = self.hdma_channels[index];
        let (pattern, pattern_len) = transfer_pattern(channel.control);
        let indirect = channel.control & 0x40 != 0;
        let direction_from_ppu = channel.control & 0x80 != 0;
        let mut source_offset = if indirect {
            state.indirect_address
        } else {
            state.table_address
        };
        let mut transfers = 0;

        for &pattern_offset in pattern.iter().take(pattern_len) {
            let ppu_address = Address24::new(
                0x002100 + u32::from(channel.destination.wrapping_add(pattern_offset)),
            );
            let source_address = if indirect {
                hdma_address(state.indirect_bank, source_offset)
            } else {
                hdma_address(channel.source_bank, source_offset)
            };
            if direction_from_ppu {
                let value = bus.read(ppu_address);
                bus.write(source_address, value);
            } else {
                let value = bus.read(source_address);
                bus.write(ppu_address, value);
            }
            transfers += 1;
            source_offset = source_offset.wrapping_add(1);
        }
        if indirect {
            self.hdma_channels[index].indirect_address = source_offset;
        } else {
            self.hdma_channels[index].table_address = source_offset;
        }
        transfers
    }
}

fn hdma_address(bank: u8, address: u16) -> Address24 {
    Address24::new((u32::from(bank) << 16) | u32::from(address))
}

/// SNES transfer patterns. The same decoder is used by general DMA and HDMA;
/// HDMA additionally uses the pattern length as the number of bytes in each
/// table entry. Patterns 6 and 7 are undocumented aliases of 2 and 3.
fn transfer_pattern(channel_control: u8) -> ([u8; 4], usize) {
    match channel_control & 0x07 {
        1 => ([0, 1, 0, 0], 2),
        2 => ([0, 0, 0, 0], 2),
        3 => ([0, 0, 1, 1], 4),
        4 => ([0, 1, 2, 3], 4),
        5 => ([0, 1, 0, 1], 4),
        6 => ([0, 0, 0, 0], 2),
        7 => ([0, 0, 1, 1], 4),
        _ => ([0, 0, 0, 0], 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::Ppu;

    struct Memory([u8; 0x10000]);

    impl CpuBus for Memory {
        fn read(&mut self, address: Address24) -> u8 {
            self.0[address.get() as usize & 0xffff]
        }

        fn write(&mut self, address: Address24, value: u8) {
            self.0[address.get() as usize & 0xffff] = value;
        }
    }

    struct PpuMemory {
        bytes: [u8; 0x10000],
        ppu: Ppu,
    }

    impl Default for PpuMemory {
        fn default() -> Self {
            Self {
                bytes: [0; 0x10000],
                ppu: Ppu::default(),
            }
        }
    }

    impl CpuBus for PpuMemory {
        fn read(&mut self, address: Address24) -> u8 {
            let address = address.get() as u16;
            match address {
                0x2139..=0x213a => self.ppu.read_register(address),
                _ => self.bytes[usize::from(address)],
            }
        }

        fn write(&mut self, address: Address24, value: u8) {
            let address = address.get() as u16;
            match address {
                0x2100..=0x213f => self.ppu.write_register(address, value),
                _ => self.bytes[usize::from(address)] = value,
            }
        }
    }

    #[test]
    fn dma_channel_moves_fixed_count_bytes() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            source_address: 0x1000,
            size: 2,
            ..DmaChannel::default()
        };
        let mut memory = Memory([0; 0x10000]);
        memory.0[0x1000] = 1;
        memory.0[0x1001] = 2;
        assert_eq!(dma.execute(1, &mut memory), 2);
        assert_eq!(memory.0[0x2100], 2);
    }

    #[test]
    fn dma_start_mask_replaces_previous_request_and_source_wraps_in_bank() {
        let mut dma = Dma::default();
        dma.write_register(0x420b, 0x03);
        dma.write_register(0x420b, 0x04);
        assert_eq!(dma.take_pending(), 0x04);
        assert_eq!(dma.take_pending(), 0);

        dma.channels[0] = DmaChannel {
            source_address: 0xffff,
            source_bank: 0x12,
            size: 2,
            ..DmaChannel::default()
        };
        let mut memory = Memory([0; 0x10000]);
        memory.0[0xffff] = 0xa5;
        memory.0[0x0000] = 0x5a;
        dma.execute(1, &mut memory);
        assert_eq!(dma.channels[0].source_address, 0x0001);
        assert_eq!(dma.channels[0].source_bank, 0x12);
    }

    #[test]
    fn dma_pattern_three_writes_b_bus_as_two_pairs() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            control: 3,
            destination: 0x20,
            source_address: 0x1000,
            size: 4,
            ..DmaChannel::default()
        };
        let mut memory = Memory([0; 0x10000]);
        memory.0[0x1000..0x1004].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(dma.execute(1, &mut memory), 4);
        assert_eq!(memory.0[0x2120], 2);
        assert_eq!(memory.0[0x2121], 4);
    }

    #[test]
    fn dma_patterns_four_through_seven_follow_the_register_table() {
        // Patterns 4-7 are valid DMAP encodings too; leaving them as the
        // mode-0 fallback silently drops three of the four B-bus writes.
        let cases = [
            (4, 4, [1, 2, 3, 4], [1, 2, 3, 4]),
            (5, 4, [1, 2, 3, 4], [3, 4, 0, 0]),
            (6, 2, [1, 2, 0, 0], [2, 0, 0, 0]),
            (7, 4, [1, 2, 3, 4], [2, 4, 0, 0]),
        ];

        for (pattern, size, source, expected) in cases {
            let mut dma = Dma::default();
            dma.channels[0] = DmaChannel {
                control: pattern,
                destination: 0x20,
                source_address: 0x1000,
                size,
                ..DmaChannel::default()
            };
            let mut memory = Memory([0; 0x10000]);
            memory.0[0x1000..0x1004].copy_from_slice(&source);

            assert_eq!(
                dma.execute(1, &mut memory),
                u32::from(size),
                "pattern {pattern}"
            );
            assert_eq!(
                &memory.0[0x2120..0x2124],
                &expected,
                "pattern {pattern} B-bus sequence"
            );
        }
    }

    #[test]
    fn dma_word_pattern_writes_both_vram_data_ports() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            control: 1,
            destination: 0x18,
            source_address: 0x1000,
            size: 2,
            ..DmaChannel::default()
        };
        let mut bus = PpuMemory::default();
        bus.ppu.write_register(0x2115, 0x80);
        bus.ppu.write_register(0x2116, 8);
        bus.ppu.write_register(0x2117, 0);
        bus.bytes[0x1000..0x1002].copy_from_slice(&[0x80, 0x80]);

        assert_eq!(dma.execute(1, &mut bus), 2);
        assert_eq!(bus.ppu.vram[8], 0x8080);
        assert_eq!(bus.ppu.vram_address, 9);
    }

    #[test]
    fn hdma_direct_repeat_entry_writes_each_scanline_then_terminates() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            control: 1,
            destination: 0x20,
            source_address: 0x1000,
            ..DmaChannel::default()
        };
        dma.hdma_enable = 1;
        let mut memory = Memory([0; 0x10000]);
        // Two-line repeat entry, followed by the zero terminator.
        memory.0[0x1000..0x1006].copy_from_slice(&[0x82, 0x11, 0x22, 0x33, 0x44, 0]);

        dma.begin_hdma_frame();
        assert_eq!(dma.run_hdma_scanline(&mut memory), 2);
        assert_eq!(memory.0[0x2120], 0x11);
        assert_eq!(memory.0[0x2121], 0x22);
        assert_eq!(dma.run_hdma_scanline(&mut memory), 2);
        assert_eq!(memory.0[0x2120], 0x33);
        assert_eq!(memory.0[0x2121], 0x44);
        assert_eq!(dma.run_hdma_scanline(&mut memory), 0);
        assert_eq!(dma.active_hdma_mask(), 0);
    }

    #[test]
    fn hdma_word_pattern_writes_vram_data_ports_and_consumes_descriptor() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            control: 1,
            destination: 0x18,
            source_address: 0x1000,
            ..DmaChannel::default()
        };
        dma.hdma_enable = 1;
        let mut bus = PpuMemory::default();
        bus.ppu.write_register(0x2115, 0x80);
        bus.ppu.write_register(0x2116, 8);
        bus.ppu.write_register(0x2117, 0);
        bus.bytes[0x1000..0x1004].copy_from_slice(&[1, 0x80, 0x80, 0]);

        assert_eq!(dma.run_hdma_scanline(&mut bus), 2);
        assert_eq!(bus.ppu.vram[8], 0x8080);
        assert_eq!(bus.ppu.vram_address, 9);
        assert_eq!(dma.run_hdma_scanline(&mut bus), 0);
        assert_eq!(dma.active_hdma_mask(), 0);
    }

    #[test]
    fn hdma_pattern_four_writes_four_consecutive_b_bus_registers() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            control: 4,
            destination: 0x20,
            source_address: 0x1000,
            ..DmaChannel::default()
        };
        dma.hdma_enable = 1;
        let mut memory = Memory([0; 0x10000]);
        // One scanline entry, followed by the table terminator.
        memory.0[0x1000..0x1006].copy_from_slice(&[0x81, 1, 2, 3, 4, 0]);

        dma.begin_hdma_frame();
        assert_eq!(dma.run_hdma_scanline(&mut memory), 4);
        assert_eq!(&memory.0[0x2120..0x2124], &[1, 2, 3, 4]);
        assert_eq!(dma.run_hdma_scanline(&mut memory), 0);
        assert_eq!(dma.active_hdma_mask(), 0);
    }

    #[test]
    fn hdma_nonrepeat_entry_waits_without_consuming_data() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            destination: 0x30,
            source_address: 0x1000,
            ..DmaChannel::default()
        };
        dma.hdma_enable = 1;
        let mut memory = Memory([0; 0x10000]);
        memory.0[0x1000..0x1005].copy_from_slice(&[2, 0x55, 2, 0x66, 0]);

        dma.begin_hdma_frame();
        assert_eq!(dma.run_hdma_scanline(&mut memory), 1);
        assert_eq!(memory.0[0x2130], 0x55);
        assert_eq!(dma.run_hdma_scanline(&mut memory), 0);
        assert_eq!(dma.hdma_channels[0].table_address, 0x1002);
        assert_eq!(dma.run_hdma_scanline(&mut memory), 1);
        assert_eq!(memory.0[0x2130], 0x66);
    }

    #[test]
    fn hdma_indirect_entry_loads_pointer_and_advances_indirect_data() {
        let mut dma = Dma::default();
        dma.channels[0] = DmaChannel {
            control: 0x40,
            destination: 0x40,
            source_address: 0x1000,
            size: 0x2000,
            ..DmaChannel::default()
        };
        dma.hdma_enable = 1;
        let mut memory = Memory([0; 0x10000]);
        dma.write_register(0x4307, 0);
        memory.0[0x1000..0x1004].copy_from_slice(&[1, 0x00, 0x20, 0]);
        memory.0[0x2000] = 0x77;

        assert_eq!(dma.run_hdma_scanline(&mut memory), 1);
        assert_eq!(memory.0[0x2140], 0x77);
        assert_eq!(dma.hdma_channels[0].indirect_address, 0x2001);
        assert_eq!(dma.hdma_channels[0].table_address, 0x1003);
    }

    #[test]
    fn dma_registers_expose_hdma_runtime_position() {
        let mut dma = Dma::default();
        dma.write_register(0x4302, 0x00);
        dma.write_register(0x4303, 0x10);
        dma.write_register(0x4304, 0x02);
        dma.write_register(0x4307, 0x03);
        dma.write_register(0x420c, 1);
        assert_eq!(dma.read_register(0x4304), 2);
        assert_eq!(dma.read_register(0x4307), 3);
        assert_eq!(dma.read_register(0x4308), 0);
        assert_eq!(dma.read_register(0x4309), 0x10);
    }
}
