//! 5A22/65C816 CPU state and semantic execution path.

use crate::{
    bus::{Address24, BusAccess, BusAccessKind, TraceSink},
    instructions::InstructionId,
};

const FLAG_CARRY: u8 = 0x01;
const FLAG_ZERO: u8 = 0x02;
const FLAG_IRQ_DISABLE: u8 = 0x04;
const FLAG_INDEX: u8 = 0x10;
const FLAG_MEMORY: u8 = 0x20;
const FLAG_OVERFLOW: u8 = 0x40;
const FLAG_NEGATIVE: u8 = 0x80;

/// Minimal bus contract used by the CPU semantic core.
pub trait CpuBus {
    fn read(&mut self, address: Address24) -> u8;
    fn write(&mut self, address: Address24, value: u8);

    fn access_master_ticks(&self, _address: Address24) -> u64 {
        6
    }
}

/// Result of one instruction execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuStep {
    pub instruction: InstructionId,
    pub cycles: u32,
    pub master_ticks: u64,
    pub access_count: u32,
}

/// Serializable CPU state boundary used by snapshots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuState {
    pub a: u16,
    pub x: u16,
    pub y: u16,
    pub sp: u16,
    pub direct_page: u16,
    pub data_bank: u8,
    pub program_bank: u8,
    pub pc: u16,
    pub status: u8,
    pub emulation_mode: bool,
    pub waiting: bool,
    pub stopped: bool,
}

/// The 5A22/65C816 register file and one semantic instruction engine.
#[derive(Clone, Debug)]
pub struct Cpu65816 {
    state: CpuState,
}

impl Default for Cpu65816 {
    fn default() -> Self {
        Self {
            state: CpuState {
                sp: 0x01ff,
                status: FLAG_IRQ_DISABLE | FLAG_INDEX | FLAG_MEMORY,
                emulation_mode: true,
                ..CpuState::default()
            },
        }
    }
}

impl Cpu65816 {
    pub(crate) fn reset(&mut self) {
        self.state = Self::default().state;
    }

    pub fn state(&self) -> CpuState {
        self.state
    }

    pub fn reset_with_bus(&mut self, bus: &mut dyn CpuBus) {
        self.reset();
        let mut trace = None;
        let mut cycles = 0;
        let mut ticks = 0;
        let mut count = 0;
        let lo = self.read(
            bus,
            Address24::new(0x00fffc),
            BusAccessKind::VectorFetch,
            &mut trace,
            &mut cycles,
            &mut ticks,
            &mut count,
        );
        let hi = self.read(
            bus,
            Address24::new(0x00fffd),
            BusAccessKind::VectorFetch,
            &mut trace,
            &mut cycles,
            &mut ticks,
            &mut count,
        );
        self.state.pc = u16::from_le_bytes([lo, hi]);
    }

    /// Executes an instruction against an empty bus. This keeps the public
    /// scaffold API useful while the complete console bus is assembled.
    pub(crate) fn step_instruction(&mut self, trace: Option<&mut dyn TraceSink>) -> CpuStep {
        let mut bus = EmptyBus;
        self.step_with_bus(&mut bus, trace)
    }

    /// Executes one instruction using the same path used by traced execution.
    pub fn step_with_bus(
        &mut self,
        bus: &mut dyn CpuBus,
        mut trace: Option<&mut dyn TraceSink>,
    ) -> CpuStep {
        let mut cycles = 0;
        let mut master_ticks = 0;
        let mut access_count = 0;
        let opcode = self.fetch_byte(
            bus,
            &mut trace,
            &mut cycles,
            &mut master_ticks,
            &mut access_count,
        );
        let instruction = InstructionId::from_opcode(opcode);

        match opcode {
            0x00 => {
                self.state.stopped = true;
                cycles = 7;
            }
            0x18 => {
                self.state.status &= !FLAG_CARRY;
                cycles = 2;
            }
            0x38 => {
                self.state.status |= FLAG_CARRY;
                cycles = 2;
            }
            0x4c => {
                let target = self.fetch_word(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = target;
                cycles = 3;
            }
            0x5c => {
                let low = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let high = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let bank = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = u16::from_le_bytes([low, high]);
                self.state.program_bank = bank;
                cycles = 4;
            }
            0x69 | 0xe9 => {
                let value = self.fetch_value(
                    bus,
                    self.accumulator_is_8_bit(),
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                if opcode == 0x69 {
                    self.adc(value);
                } else {
                    self.sbc(value);
                }
                cycles = 2 + u32::from(!self.accumulator_is_8_bit());
            }
            0x80 => {
                let offset = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                ) as i8;
                self.state.pc = self.state.pc.wrapping_add_signed(i16::from(offset));
                cycles = 3;
            }
            0x88 | 0xca | 0xc8 | 0xe8 => {
                let index_is_x = opcode == 0xca || opcode == 0xe8;
                let increment = opcode == 0xc8 || opcode == 0xe8;
                let value = if index_is_x {
                    self.state.x
                } else {
                    self.state.y
                };
                let result = if increment {
                    value.wrapping_add(1)
                } else {
                    value.wrapping_sub(1)
                };
                if index_is_x {
                    self.state.x = self.set_index_width(result);
                } else {
                    self.state.y = self.set_index_width(result);
                }
                self.set_zero_negative(result, self.index_is_8_bit());
                cycles = 2;
            }
            0x8a | 0x98 => {
                let value = if opcode == 0x8a {
                    self.state.x
                } else {
                    self.state.y
                };
                self.set_accumulator(value);
                self.set_zero_negative(value, self.accumulator_is_8_bit());
                cycles = 2;
            }
            0x8d | 0x9c => {
                let address = self.fetch_word(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let value = if opcode == 0x8d { self.state.a } else { 0 };
                self.write_value(
                    bus,
                    Address24::new((u32::from(self.state.data_bank) << 16) | u32::from(address)),
                    value,
                    self.accumulator_is_8_bit(),
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                cycles = 4 + u32::from(!self.accumulator_is_8_bit());
            }
            0x8f => {
                let low = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let high = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let bank = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.write_value(
                    bus,
                    Address24::new(
                        (u32::from(bank) << 16) | u32::from(u16::from_le_bytes([low, high])),
                    ),
                    self.state.a,
                    self.accumulator_is_8_bit(),
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                cycles = 5 + u32::from(!self.accumulator_is_8_bit());
            }
            0xa0 | 0xa2 => {
                let value = self.fetch_value(
                    bus,
                    self.index_is_8_bit(),
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                if opcode == 0xa0 {
                    self.state.y = self.set_index_width(value);
                } else {
                    self.state.x = self.set_index_width(value);
                }
                self.set_zero_negative(value, self.index_is_8_bit());
                cycles = 2 + u32::from(!self.index_is_8_bit());
            }
            0xa8 | 0xaa => {
                let value = self.state.a;
                if opcode == 0xa8 {
                    self.state.y = self.set_index_width(value);
                } else {
                    self.state.x = self.set_index_width(value);
                }
                self.set_zero_negative(value, self.index_is_8_bit());
                cycles = 2;
            }
            0xa9 => {
                let value = self.fetch_value(
                    bus,
                    self.accumulator_is_8_bit(),
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.set_accumulator(value);
                self.set_zero_negative(value, self.accumulator_is_8_bit());
                cycles = 2 + u32::from(!self.accumulator_is_8_bit());
            }
            0xc2 | 0xe2 => {
                let mask = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                if opcode == 0xc2 {
                    self.state.status &= !mask;
                } else {
                    self.state.status |= mask;
                }
                self.normalize_widths();
                cycles = 3;
            }
            0xea => cycles = 2,
            0xeb => {
                self.state.a = self.state.a.rotate_left(8);
                self.state.status = (self.state.status & !(FLAG_NEGATIVE | FLAG_ZERO))
                    | if self.state.a & 0x00ff == 0 {
                        FLAG_ZERO
                    } else {
                        0
                    }
                    | if self.state.a & 0x0080 != 0 {
                        FLAG_NEGATIVE
                    } else {
                        0
                    };
                cycles = 3;
            }
            0xfb => {
                let carry = self.state.status & FLAG_CARRY != 0;
                let old_emulation = self.state.emulation_mode;
                self.state.emulation_mode = carry;
                if carry {
                    self.state.status |= FLAG_MEMORY | FLAG_INDEX;
                }
                self.state.status =
                    (self.state.status & !FLAG_CARRY) | if old_emulation { FLAG_CARRY } else { 0 };
                self.normalize_widths();
                cycles = 2;
            }
            _ => cycles = 2,
        }

        CpuStep {
            instruction,
            cycles,
            master_ticks: master_ticks + u64::from(cycles.saturating_sub(access_count * 1)) * 6,
            access_count,
        }
    }

    pub(crate) fn snapshot(&self) -> CpuState {
        self.state
    }

    pub(crate) fn restore(&mut self, state: CpuState) {
        self.state = state;
    }

    fn accumulator_is_8_bit(&self) -> bool {
        self.state.emulation_mode || self.state.status & FLAG_MEMORY != 0
    }
    fn index_is_8_bit(&self) -> bool {
        self.state.emulation_mode || self.state.status & FLAG_INDEX != 0
    }

    fn normalize_widths(&mut self) {
        if self.index_is_8_bit() {
            self.state.x &= 0x00ff;
            self.state.y &= 0x00ff;
        }
        if self.state.emulation_mode {
            self.state.sp = 0x0100 | (self.state.sp & 0x00ff);
        }
    }

    fn set_accumulator(&mut self, value: u16) {
        if self.accumulator_is_8_bit() {
            self.state.a = (self.state.a & 0xff00) | (value & 0x00ff);
        } else {
            self.state.a = value;
        }
    }

    fn set_index_width(&self, value: u16) -> u16 {
        if self.index_is_8_bit() {
            value & 0x00ff
        } else {
            value
        }
    }

    fn set_zero_negative(&mut self, value: u16, eight_bit: bool) {
        let mask = if eight_bit { 0x00ff } else { 0xffff };
        let sign = if eight_bit { 0x0080 } else { 0x8000 };
        self.state.status = (self.state.status & !(FLAG_ZERO | FLAG_NEGATIVE))
            | if value & mask == 0 { FLAG_ZERO } else { 0 }
            | if value & sign != 0 { FLAG_NEGATIVE } else { 0 };
    }

    fn adc(&mut self, value: u16) {
        let width = if self.accumulator_is_8_bit() { 8 } else { 16 };
        let mask = (1u32 << width) - 1;
        let lhs = u32::from(self.state.a & mask as u16);
        let rhs = u32::from(value & mask as u16);
        let carry = u32::from(self.state.status & FLAG_CARRY != 0);
        let result = lhs + rhs + carry;
        let truncated = result & mask;
        let sign = 1u32 << (width - 1);
        self.state.status = (self.state.status & !(FLAG_CARRY | FLAG_OVERFLOW))
            | if result > mask { FLAG_CARRY } else { 0 }
            | if (!(lhs ^ rhs) & (lhs ^ truncated) & sign) != 0 {
                FLAG_OVERFLOW
            } else {
                0
            };
        self.set_accumulator(truncated as u16);
        self.set_zero_negative(truncated as u16, width == 8);
    }

    fn sbc(&mut self, value: u16) {
        let width = if self.accumulator_is_8_bit() { 8 } else { 16 };
        let mask = (1u32 << width) - 1;
        let lhs = u32::from(self.state.a & mask as u16);
        let rhs = u32::from(value & mask as u16);
        let borrow = u32::from(self.state.status & FLAG_CARRY == 0);
        let result = lhs.wrapping_sub(rhs).wrapping_sub(borrow);
        let truncated = result & mask;
        let sign = 1u32 << (width - 1);
        self.state.status = (self.state.status & !(FLAG_CARRY | FLAG_OVERFLOW))
            | if lhs >= rhs + borrow { FLAG_CARRY } else { 0 }
            | if ((lhs ^ truncated) & (lhs ^ rhs) & sign) != 0 {
                FLAG_OVERFLOW
            } else {
                0
            };
        self.set_accumulator(truncated as u16);
        self.set_zero_negative(truncated as u16, width == 8);
    }

    fn fetch_byte(
        &mut self,
        bus: &mut dyn CpuBus,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> u8 {
        let address =
            Address24::new((u32::from(self.state.program_bank) << 16) | u32::from(self.state.pc));
        let value = self.read(
            bus,
            address,
            BusAccessKind::OpcodeFetch,
            trace,
            cycles,
            ticks,
            count,
        );
        self.state.pc = self.state.pc.wrapping_add(1);
        value
    }

    fn fetch_word(
        &mut self,
        bus: &mut dyn CpuBus,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> u16 {
        let lo = self.fetch_byte(bus, trace, cycles, ticks, count);
        let hi = self.fetch_byte(bus, trace, cycles, ticks, count);
        u16::from_le_bytes([lo, hi])
    }

    fn fetch_value(
        &mut self,
        bus: &mut dyn CpuBus,
        wide: bool,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> u16 {
        let lo = self.fetch_byte(bus, trace, cycles, ticks, count);
        if wide {
            u16::from(lo)
        } else {
            u16::from(lo) | u16::from(self.fetch_byte(bus, trace, cycles, ticks, count)) << 8
        }
    }

    fn read(
        &mut self,
        bus: &mut dyn CpuBus,
        address: Address24,
        kind: BusAccessKind,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> u8 {
        let value = bus.read(address);
        *cycles += 1;
        *ticks += bus.access_master_ticks(address);
        *count += 1;
        if let Some(sink) = trace.as_deref_mut() {
            sink.record(BusAccess {
                kind,
                address,
                value: Some(value),
            });
        }
        value
    }

    fn write_value(
        &mut self,
        bus: &mut dyn CpuBus,
        address: Address24,
        value: u16,
        eight_bit: bool,
        trace: &mut Option<&mut dyn TraceSink>,
        ticks: &mut u64,
        count: &mut u32,
    ) {
        bus.write(address, value as u8);
        *ticks += bus.access_master_ticks(address);
        *count += 1;
        if let Some(sink) = trace.as_deref_mut() {
            sink.record(BusAccess {
                kind: BusAccessKind::DataWrite,
                address,
                value: Some(value as u8),
            });
        }
        if !eight_bit {
            let next = Address24::new((address.get() & 0xff0000) | ((address.get() + 1) & 0xffff));
            bus.write(next, (value >> 8) as u8);
            *ticks += bus.access_master_ticks(next);
            *count += 1;
            if let Some(sink) = trace.as_deref_mut() {
                sink.record(BusAccess {
                    kind: BusAccessKind::DataWrite,
                    address: next,
                    value: Some((value >> 8) as u8),
                });
            }
        }
    }
}

struct EmptyBus;

impl CpuBus for EmptyBus {
    fn read(&mut self, _address: Address24) -> u8 {
        0
    }
    fn write(&mut self, _address: Address24, _value: u8) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FlatBus {
        memory: Vec<u8>,
    }

    impl FlatBus {
        fn new() -> Self {
            Self {
                memory: vec![0; 1 << 24],
            }
        }
    }

    impl CpuBus for FlatBus {
        fn read(&mut self, address: Address24) -> u8 {
            self.memory[address.get() as usize]
        }

        fn write(&mut self, address: Address24, value: u8) {
            self.memory[address.get() as usize] = value;
        }
    }

    #[test]
    fn reset_reads_the_emulation_vector() {
        let mut bus = FlatBus::new();
        bus.memory[0x00fffc] = 0x34;
        bus.memory[0x00fffd] = 0x12;
        let mut cpu = Cpu65816::default();
        cpu.reset_with_bus(&mut bus);
        assert_eq!(cpu.state().pc, 0x1234);
        assert!(cpu.state().emulation_mode);
        assert_eq!(
            cpu.state().status & (FLAG_MEMORY | FLAG_INDEX),
            FLAG_MEMORY | FLAG_INDEX
        );
    }

    #[test]
    fn immediate_load_and_absolute_store_follow_trace_path() {
        let mut bus = FlatBus::new();
        bus.memory[0] = 0xa9;
        bus.memory[1] = 0x42;
        bus.memory[2] = 0x8d;
        bus.memory[3] = 0x00;
        bus.memory[4] = 0x20;
        let mut cpu = Cpu65816::default();
        let mut trace = crate::VecTrace::default();

        let load = cpu.step_with_bus(&mut bus, Some(&mut trace));
        assert_eq!(load.instruction, InstructionId::LdaImmediate);
        assert_eq!(cpu.state().a & 0xff, 0x42);
        assert_eq!(trace.as_slice().len(), 2);

        let store = cpu.step_with_bus(&mut bus, Some(&mut trace));
        assert_eq!(store.instruction, InstructionId::StaAbsolute);
        assert_eq!(bus.memory[0x2000], 0x42);
        assert_eq!(store.access_count, 4);
    }

    #[test]
    fn xce_and_rep_enable_native_16_bit_accumulator() {
        let mut bus = FlatBus::new();
        bus.memory[..8].copy_from_slice(&[0x18, 0xfb, 0xc2, 0x20, 0xa9, 0x34, 0x12, 0xea]);
        let mut cpu = Cpu65816::default();
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        assert!(!cpu.state().emulation_mode);
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        assert_eq!(cpu.state().a, 0x1234);
    }
}
