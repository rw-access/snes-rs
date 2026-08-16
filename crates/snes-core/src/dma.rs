//! General DMA channel state and transfer engine.

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

/// General DMA controller state. HDMA register storage is retained for the
/// next timing increment, while ordinary DMA is fully transferable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Dma {
    pub channels: [DmaChannel; 8],
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
                0x3 => state.source_address = (state.source_address & 0x00ff) | (u16::from(value) << 8),
                0x4 => state.source_bank = value,
                0x5 => state.size = (state.size & 0xff00) | u16::from(value),
                0x6 => state.size = (state.size & 0x00ff) | (u16::from(value) << 8),
                _ => {}
            }
        } else if address == 0x420c {
            self.hdma_enable = value;
        } else if address == 0x420b {
            self.pending |= value;
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
        let mut source = Address24::new(
            (u32::from(channel.source_bank) << 16) | u32::from(channel.source_address),
        );
        let count = if channel.size == 0 { 0x1_0000 } else { u32::from(channel.size) };
        let direction_from_ppu = channel.control & 0x80 != 0;
        let fixed_source = channel.control & 0x08 != 0;
        let decrement_source = channel.control & 0x10 != 0;
        let pattern = match channel.control & 0x07 {
            1 => [0u8, 1, 0, 1],
            2 => [0u8, 0, 1, 1],
            3 => [0u8, 1, 1, 0],
            _ => [0u8, 0, 0, 0],
        };
        let mut transfers = 0;
        for offset in 0..count {
            let ppu_address = Address24::new(
                0x002100 + u32::from(channel.destination) + u32::from(pattern[(offset & 3) as usize]),
            );
            if direction_from_ppu {
                let value = bus.read(ppu_address);
                bus.write(source, value);
            } else {
                let value = bus.read(source);
                bus.write(ppu_address, value);
            }
            transfers += 1;
            if !fixed_source {
                source = if decrement_source {
                    Address24::new(source.get().wrapping_sub(1))
                } else {
                    Address24::new(source.get().wrapping_add(1))
                };
            }
        }
        self.channels[index].source_address = source.get() as u16;
        self.channels[index].source_bank = (source.get() >> 16) as u8;
        self.channels[index].size = 0;
        transfers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Memory([u8; 0x10000]);

    impl CpuBus for Memory {
        fn read(&mut self, address: Address24) -> u8 {
            self.0[address.get() as usize & 0xffff]
        }

        fn write(&mut self, address: Address24, value: u8) {
            self.0[address.get() as usize & 0xffff] = value;
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
}
