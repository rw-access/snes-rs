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

    /// Executes one instruction using the same path used by traced execution.
    pub fn step_with_bus(
        &mut self,
        bus: &mut dyn CpuBus,
        mut trace: Option<&mut dyn TraceSink>,
    ) -> CpuStep {
        let mut cycles = 0;
        let mut master_ticks = 0;
        let mut access_count = 0;
        let opcode = self.fetch_opcode(
            bus,
            &mut trace,
            &mut cycles,
            &mut master_ticks,
            &mut access_count,
        );
        let instruction = InstructionId::from_opcode(opcode);

        match opcode {
            0x00 => {
                self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.push_word(
                    bus,
                    self.state.pc,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.push_status(bus, &mut trace, &mut master_ticks, &mut access_count);
                self.state.status |= FLAG_IRQ_DISABLE;
                self.state.status &= !0x08;
                self.state.program_bank = 0;
                let lo = self.read(
                    bus,
                    Address24::new(0x00fffe),
                    BusAccessKind::VectorFetch,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let hi = self.read(
                    bus,
                    Address24::new(0x00ffff),
                    BusAccessKind::VectorFetch,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = u16::from_le_bytes([lo, hi]);
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
            0x20 => {
                let target = self.fetch_word(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.push_word(
                    bus,
                    self.state.pc.wrapping_sub(1),
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = target;
                cycles = 6;
            }
            0x22 => {
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
                self.push_byte(
                    bus,
                    self.state.program_bank,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.push_word(
                    bus,
                    self.state.pc.wrapping_sub(1),
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = u16::from_le_bytes([low, high]);
                self.state.program_bank = bank;
                cycles = 8;
            }
            0x40 => {
                self.state.status = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                let lo = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                let hi = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = u16::from_le_bytes([lo, hi]);
                if !self.state.emulation_mode {
                    self.state.program_bank = self.pop_byte(
                        bus,
                        &mut trace,
                        &mut master_ticks,
                        &mut access_count,
                    );
                }
                self.normalize_widths();
                cycles = 6;
            }
            0x60 => {
                let lo = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                let hi = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = u16::from_le_bytes([lo, hi]).wrapping_add(1);
                cycles = 6;
            }
            0x6b => {
                let lo = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                let hi = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                let bank = self.pop_byte(
                    bus,
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.state.pc = u16::from_le_bytes([lo, hi]).wrapping_add(1);
                self.state.program_bank = bank;
                cycles = 6;
            }
            0x09 | 0x29 | 0x49 | 0x69 | 0xe9 => {
                let value = self.fetch_value(
                    bus,
                    self.accumulator_is_8_bit(),
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                match opcode {
                    0x09 => self.logic_acc(value, |lhs, rhs| lhs | rhs),
                    0x29 => self.logic_acc(value, |lhs, rhs| lhs & rhs),
                    0x49 => self.logic_acc(value, |lhs, rhs| lhs ^ rhs),
                    0x69 => self.adc(value),
                    _ => self.sbc(value),
                }
                cycles = 2 + u32::from(!self.accumulator_is_8_bit());
            }
            0x05 | 0x25 | 0x45 | 0x65 | 0xc5 | 0xe5 | 0x0d | 0x2d | 0x4d | 0x6d | 0xcd
            | 0xed | 0x0f | 0x2f | 0x4f | 0x6f | 0xcf | 0xef => {
                let addressing = opcode & 0x0f;
                let long = addressing == 0x0f;
                let address = if long {
                    self.long_address(
                        bus,
                        &mut trace,
                        &mut cycles,
                        &mut master_ticks,
                        &mut access_count,
                    )
                } else if addressing == 0x0d {
                    self.absolute_address(
                        bus,
                        &mut trace,
                        &mut cycles,
                        &mut master_ticks,
                        &mut access_count,
                    )
                } else {
                    self.direct_address(
                        bus,
                        &mut trace,
                        &mut cycles,
                        &mut master_ticks,
                        &mut access_count,
                    )
                };
                let value = self.read_value(
                    bus,
                    address,
                    self.accumulator_is_8_bit(),
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                match opcode & 0xe0 {
                    0x00 => self.logic_acc(value, |lhs, rhs| lhs | rhs),
                    0x20 => self.logic_acc(value, |lhs, rhs| lhs & rhs),
                    0x40 => self.logic_acc(value, |lhs, rhs| lhs ^ rhs),
                    0x60 => self.adc(value),
                    0xc0 => self.compare(self.state.a, value, self.accumulator_is_8_bit()),
                    _ => self.sbc(value),
                }
                cycles = if long { 5 } else if addressing == 0x0d { 4 } else { 3 };
            }
            0xc0 | 0xc9 | 0xe0 => {
                let width = if opcode == 0xe0 {
                    self.index_is_8_bit()
                } else {
                    self.accumulator_is_8_bit()
                };
                let value = self.fetch_value(
                    bus,
                    width,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let lhs = if opcode == 0xc0 {
                    self.state.y
                } else if opcode == 0xe0 {
                    self.state.x
                } else {
                    self.state.a
                };
                self.compare(lhs, value, width);
                cycles = 2 + u32::from(!width);
            }
            0x10 | 0x30 | 0x50 | 0x70 | 0x80 | 0x90 | 0xb0 | 0xd0 | 0xf0 => {
                let offset = self.fetch_byte(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                ) as i8;
                let take = match opcode {
                    0x10 => self.state.status & FLAG_NEGATIVE == 0,
                    0x30 => self.state.status & FLAG_NEGATIVE != 0,
                    0x50 => self.state.status & FLAG_OVERFLOW == 0,
                    0x70 => self.state.status & FLAG_OVERFLOW != 0,
                    0x80 => true,
                    0x90 => self.state.status & FLAG_CARRY == 0,
                    0xb0 => self.state.status & FLAG_CARRY != 0,
                    0xd0 => self.state.status & FLAG_ZERO == 0,
                    _ => self.state.status & FLAG_ZERO != 0,
                };
                if take {
                    self.state.pc = self.state.pc.wrapping_add_signed(i16::from(offset));
                }
                cycles = 2 + u32::from(take);
                if take && !self.same_page(self.state.pc, self.state.pc.wrapping_sub_signed(i16::from(offset))) {
                    cycles += 1;
                }
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
            0x64 | 0x85 | 0xa5 => {
                let address = self.direct_address(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                if opcode == 0xa5 {
                    let value = self.read_value(
                        bus,
                        address,
                        self.accumulator_is_8_bit(),
                        &mut trace,
                        &mut cycles,
                        &mut master_ticks,
                        &mut access_count,
                    );
                    self.set_accumulator(value);
                    self.set_zero_negative(value, self.accumulator_is_8_bit());
                } else {
                    self.write_value(
                        bus,
                        address,
                        if opcode == 0x85 { self.state.a } else { 0 },
                        self.accumulator_is_8_bit(),
                        &mut trace,
                        &mut master_ticks,
                        &mut access_count,
                    );
                }
                cycles = if opcode == 0xa5 { 3 } else { 3 };
            }
            0x84 | 0x86 => {
                let address = self.direct_address(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let value = if opcode == 0x84 { self.state.y } else { self.state.x };
                self.write_value(
                    bus,
                    address,
                    value,
                    self.index_is_8_bit(),
                    &mut trace,
                    &mut master_ticks,
                    &mut access_count,
                );
                cycles = 3;
            }
            0xa4 | 0xa6 => {
                let address = self.direct_address(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let value = self.read_value(
                    bus,
                    address,
                    self.index_is_8_bit(),
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                if opcode == 0xa4 {
                    self.state.y = self.set_index_width(value);
                } else {
                    self.state.x = self.set_index_width(value);
                }
                self.set_zero_negative(value, self.index_is_8_bit());
                cycles = 3;
            }
            0x8c | 0x8e | 0xac | 0xae | 0xad => {
                let address = self.absolute_address(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                if opcode == 0xac || opcode == 0xae || opcode == 0xad {
                    let value = self.read_value(
                        bus,
                        address,
                        self.index_is_8_bit(),
                        &mut trace,
                        &mut cycles,
                        &mut master_ticks,
                        &mut access_count,
                    );
                    if opcode == 0xac {
                        self.state.y = self.set_index_width(value);
                    } else if opcode == 0xae {
                        self.state.x = self.set_index_width(value);
                    } else {
                        self.set_accumulator(value);
                        self.set_zero_negative(value, self.accumulator_is_8_bit());
                    }
                    if opcode != 0xad {
                        self.set_zero_negative(value, self.index_is_8_bit());
                    }
                } else {
                    let value = if opcode == 0x8c { self.state.y } else { self.state.x };
                    self.write_value(
                        bus,
                        address,
                        value,
                        self.index_is_8_bit(),
                        &mut trace,
                        &mut master_ticks,
                        &mut access_count,
                    );
                }
                cycles = if opcode == 0xad { 4 } else { 4 };
            }
            0xaf => {
                let address = self.long_address(
                    bus,
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                let value = self.read_value(
                    bus,
                    address,
                    self.accumulator_is_8_bit(),
                    &mut trace,
                    &mut cycles,
                    &mut master_ticks,
                    &mut access_count,
                );
                self.set_accumulator(value);
                self.set_zero_negative(value, self.accumulator_is_8_bit());
                cycles = 5;
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
            0x08 | 0x48 | 0x5a | 0xda => {
                match opcode {
                    0x08 => self.push_status(
                        bus,
                        &mut trace,
                        &mut master_ticks,
                        &mut access_count,
                    ),
                    0x48 => self.push_word_or_byte(
                        bus,
                        self.state.a,
                        self.accumulator_is_8_bit(),
                        &mut trace,
                        &mut master_ticks,
                        &mut access_count,
                    ),
                    0x5a => self.push_word_or_byte(
                        bus,
                        self.state.y,
                        self.index_is_8_bit(),
                        &mut trace,
                        &mut master_ticks,
                        &mut access_count,
                    ),
                    _ => self.push_word_or_byte(
                        bus,
                        self.state.x,
                        self.index_is_8_bit(),
                        &mut trace,
                        &mut master_ticks,
                        &mut access_count,
                    ),
                }
                cycles = 3;
            }
            0x28 | 0x68 | 0x7a | 0xfa => {
                match opcode {
                    0x28 => {
                        self.state.status = self.pop_byte(
                            bus,
                            &mut trace,
                            &mut master_ticks,
                            &mut access_count,
                        );
                        self.normalize_widths();
                    }
                    0x68 => {
                        let value = self.pop_word_or_byte(
                            bus,
                            self.accumulator_is_8_bit(),
                            &mut trace,
                            &mut master_ticks,
                            &mut access_count,
                        );
                        self.set_accumulator(value);
                        self.set_zero_negative(value, self.accumulator_is_8_bit());
                    }
                    0x7a => {
                        let value = self.pop_word_or_byte(
                            bus,
                            self.index_is_8_bit(),
                            &mut trace,
                            &mut master_ticks,
                            &mut access_count,
                        );
                        self.state.y = self.set_index_width(value);
                        self.set_zero_negative(value, self.index_is_8_bit());
                    }
                    _ => {
                        let value = self.pop_word_or_byte(
                            bus,
                            self.index_is_8_bit(),
                            &mut trace,
                            &mut master_ticks,
                            &mut access_count,
                        );
                        self.state.x = self.set_index_width(value);
                        self.set_zero_negative(value, self.index_is_8_bit());
                    }
                }
                cycles = 4;
            }
            0x1a | 0x3a => {
                let value = if opcode == 0x1a {
                    self.state.a.wrapping_add(1)
                } else {
                    self.state.a.wrapping_sub(1)
                };
                self.set_accumulator(value);
                self.set_zero_negative(value, self.accumulator_is_8_bit());
                cycles = 2;
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
            0x58 | 0x78 | 0xb8 | 0xd8 | 0xf8 => {
                match opcode {
                    0x58 => self.state.status &= !FLAG_IRQ_DISABLE,
                    0x78 => self.state.status |= FLAG_IRQ_DISABLE,
                    0xb8 => self.state.status &= !FLAG_OVERFLOW,
                    0xd8 => self.state.status &= !0x08,
                    _ => self.state.status |= 0x08,
                }
                cycles = 2;
            }
            0x1b | 0x3b | 0x5b | 0x7b | 0x9b | 0xbb | 0x9a | 0xba => {
                match opcode {
                    0x1b => {
                        self.state.sp = self.state.a;
                        self.normalize_widths();
                    }
                    0x3b => {
                        self.state.a = self.state.sp;
                        self.set_zero_negative(self.state.a, self.accumulator_is_8_bit());
                    }
                    0x5b => {
                        self.state.direct_page = self.state.a;
                        self.set_zero_negative(self.state.a, false);
                    }
                    0x7b => {
                        self.state.a = self.state.direct_page;
                        self.set_zero_negative(self.state.a, self.accumulator_is_8_bit());
                    }
                    0x9b => {
                        self.state.y = self.state.x;
                        self.set_zero_negative(self.state.y, self.index_is_8_bit());
                    }
                    0xbb => {
                        self.state.x = self.state.y;
                        self.set_zero_negative(self.state.x, self.index_is_8_bit());
                    }
                    0x9a => {
                        self.state.sp = if self.state.emulation_mode {
                            0x0100 | (self.state.x & 0x00ff)
                        } else {
                            self.state.x
                        };
                    }
                    _ => {
                        self.state.x = if self.state.emulation_mode {
                            self.state.sp & 0x00ff
                        } else {
                            self.state.sp
                        };
                        self.set_zero_negative(self.state.x, self.index_is_8_bit());
                    }
                }
                cycles = 2;
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
            0xdb => {
                self.state.stopped = true;
                cycles = 3;
            }
            0xcb => {
                self.state.waiting = true;
                cycles = 3;
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
        let binary_result = lhs + rhs + carry;
        let (truncated, decimal_carry) = if self.state.status & 0x08 != 0 {
            let mut decimal = lhs + rhs + carry;
            let mut shift = 0;
            while shift < width {
                let nibble = ((decimal >> shift) & 0x0f) as u8;
                if nibble > 9 {
                    decimal += 6 << shift;
                }
                shift += 4;
            }
            (decimal & mask, decimal > mask)
        } else {
            (binary_result & mask, binary_result > mask)
        };
        let sign = 1u32 << (width - 1);
        self.state.status = (self.state.status & !(FLAG_CARRY | FLAG_OVERFLOW))
            | if decimal_carry {
                FLAG_CARRY
            } else {
                0
            }
            | if (!(lhs ^ rhs) & (lhs ^ (binary_result & mask)) & sign) != 0 {
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
        let binary_result = lhs.wrapping_sub(rhs).wrapping_sub(borrow);
        let truncated = if self.state.status & 0x08 != 0 {
            let mut decimal = 0u32;
            let mut borrow_digit = borrow;
            let mut shift = 0;
            while shift < width {
                let left = ((lhs >> shift) & 0x0f) as i32;
                let right = ((rhs >> shift) & 0x0f) as i32;
                let mut digit = left - right - borrow_digit as i32;
                if digit < 0 {
                    digit += 10;
                    borrow_digit = 1;
                } else {
                    borrow_digit = 0;
                }
                decimal |= (digit as u32) << shift;
                shift += 4;
            }
            decimal
        } else {
            binary_result & mask
        };
        let sign = 1u32 << (width - 1);
        self.state.status = (self.state.status & !(FLAG_CARRY | FLAG_OVERFLOW))
            | if if self.state.status & 0x08 != 0 {
                lhs >= rhs + borrow
            } else {
                lhs >= rhs + borrow
            } {
                FLAG_CARRY
            } else {
                0
            }
            | if ((lhs ^ (binary_result & mask)) & (lhs ^ rhs) & sign) != 0 {
                FLAG_OVERFLOW
            } else {
                0
            };
        self.set_accumulator(truncated as u16);
        self.set_zero_negative(truncated as u16, width == 8);
    }

    fn logic_acc(&mut self, value: u16, operation: impl FnOnce(u16, u16) -> u16) {
        let mask = if self.accumulator_is_8_bit() { 0x00ff } else { 0xffff };
        let result = operation(self.state.a & mask, value & mask);
        self.set_accumulator(result);
        self.set_zero_negative(result, mask == 0x00ff);
    }

    fn compare(&mut self, lhs: u16, rhs: u16, eight_bit: bool) {
        let mask = if eight_bit { 0x00ff } else { 0xffff };
        let left = lhs & mask;
        let right = rhs & mask;
        self.state.status = (self.state.status & !FLAG_CARRY)
            | if left >= right { FLAG_CARRY } else { 0 };
        self.set_zero_negative(left.wrapping_sub(right), eight_bit);
    }

    fn same_page(&self, lhs: u16, rhs: u16) -> bool {
        lhs & 0xff00 == rhs & 0xff00
    }

    fn direct_address(
        &mut self,
        bus: &mut dyn CpuBus,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> Address24 {
        let offset = self.fetch_byte(bus, trace, cycles, ticks, count);
        Address24::new(u32::from(self.state.direct_page.wrapping_add(u16::from(offset))))
    }

    fn absolute_address(
        &mut self,
        bus: &mut dyn CpuBus,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> Address24 {
        let offset = self.fetch_word(bus, trace, cycles, ticks, count);
        Address24::new((u32::from(self.state.data_bank) << 16) | u32::from(offset))
    }

    fn long_address(
        &mut self,
        bus: &mut dyn CpuBus,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> Address24 {
        let low = self.fetch_byte(bus, trace, cycles, ticks, count);
        let high = self.fetch_byte(bus, trace, cycles, ticks, count);
        let bank = self.fetch_byte(bus, trace, cycles, ticks, count);
        Address24::new(
            (u32::from(bank) << 16) | u32::from(u16::from_le_bytes([low, high])),
        )
    }

    fn read_value(
        &mut self,
        bus: &mut dyn CpuBus,
        address: Address24,
        eight_bit: bool,
        trace: &mut Option<&mut dyn TraceSink>,
        cycles: &mut u32,
        ticks: &mut u64,
        count: &mut u32,
    ) -> u16 {
        let low = self.read(
            bus,
            address,
            BusAccessKind::DataRead,
            trace,
            cycles,
            ticks,
            count,
        );
        if eight_bit {
            u16::from(low)
        } else {
            let next = Address24::new((address.get() & 0xff0000) | ((address.get() + 1) & 0xffff));
            u16::from(low)
                | u16::from(self.read(
                    bus,
                    next,
                    BusAccessKind::DataRead,
                    trace,
                    cycles,
                    ticks,
                    count,
                )) << 8
        }
    }

    fn push_byte(
        &mut self,
        bus: &mut dyn CpuBus,
        value: u8,
        trace: &mut Option<&mut dyn TraceSink>,
        ticks: &mut u64,
        count: &mut u32,
    ) {
        let address = self.stack_address();
        bus.write(address, value);
        *ticks += bus.access_master_ticks(address);
        *count += 1;
        if let Some(sink) = trace.as_deref_mut() {
            sink.record(BusAccess {
                kind: BusAccessKind::DataWrite,
                address,
                value: Some(value),
            });
        }
        self.state.sp = if self.state.emulation_mode {
            0x0100 | (self.state.sp as u8).wrapping_sub(1) as u16
        } else {
            self.state.sp.wrapping_sub(1)
        };
    }

    fn pop_byte(
        &mut self,
        bus: &mut dyn CpuBus,
        trace: &mut Option<&mut dyn TraceSink>,
        ticks: &mut u64,
        count: &mut u32,
    ) -> u8 {
        self.state.sp = if self.state.emulation_mode {
            0x0100 | (self.state.sp as u8).wrapping_add(1) as u16
        } else {
            self.state.sp.wrapping_add(1)
        };
        let address = self.stack_address();
        let value = bus.read(address);
        *ticks += bus.access_master_ticks(address);
        *count += 1;
        if let Some(sink) = trace.as_deref_mut() {
            sink.record(BusAccess {
                kind: BusAccessKind::DataRead,
                address,
                value: Some(value),
            });
        }
        value
    }

    fn stack_address(&self) -> Address24 {
        Address24::new(if self.state.emulation_mode {
            0x000100 | u32::from(self.state.sp & 0x00ff)
        } else {
            u32::from(self.state.sp)
        })
    }

    fn push_word(
        &mut self,
        bus: &mut dyn CpuBus,
        value: u16,
        trace: &mut Option<&mut dyn TraceSink>,
        ticks: &mut u64,
        count: &mut u32,
    ) {
        self.push_byte(bus, (value >> 8) as u8, trace, ticks, count);
        self.push_byte(bus, value as u8, trace, ticks, count);
    }

    fn push_status(
        &mut self,
        bus: &mut dyn CpuBus,
        trace: &mut Option<&mut dyn TraceSink>,
        ticks: &mut u64,
        count: &mut u32,
    ) {
        self.push_byte(bus, self.state.status | 0x10, trace, ticks, count);
    }

    fn push_word_or_byte(
        &mut self,
        bus: &mut dyn CpuBus,
        value: u16,
        eight_bit: bool,
        trace: &mut Option<&mut dyn TraceSink>,
        ticks: &mut u64,
        count: &mut u32,
    ) {
        if eight_bit {
            self.push_byte(bus, value as u8, trace, ticks, count);
        } else {
            self.push_word(bus, value, trace, ticks, count);
        }
    }

    fn pop_word_or_byte(
        &mut self,
        bus: &mut dyn CpuBus,
        eight_bit: bool,
        trace: &mut Option<&mut dyn TraceSink>,
        ticks: &mut u64,
        count: &mut u32,
    ) -> u16 {
        let low = self.pop_byte(bus, trace, ticks, count);
        if eight_bit {
            u16::from(low)
        } else {
            u16::from(low) | u16::from(self.pop_byte(bus, trace, ticks, count)) << 8
        }
    }

    fn fetch_opcode(
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
            BusAccessKind::OperandFetch,
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

    #[test]
    fn stack_round_trip_and_subroutine_return_work() {
        let mut bus = FlatBus::new();
        bus.memory[..8].copy_from_slice(&[0xa9, 0x42, 0x48, 0xa9, 0x00, 0x68, 0xea, 0xea]);
        let mut cpu = Cpu65816::default();
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        assert_eq!(cpu.state().a & 0xff, 0x42);

        bus.memory[..8].copy_from_slice(&[0x20, 0x05, 0x00, 0xea, 0xea, 0xa9, 0x07, 0x60]);
        cpu = Cpu65816::default();
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        assert_eq!(cpu.state().a & 0xff, 0x07);
        assert_eq!(cpu.state().pc, 0x0003);
    }

    #[test]
    fn conditional_branch_observes_status_flags() {
        let mut bus = FlatBus::new();
        bus.memory[..6].copy_from_slice(&[0x18, 0x90, 0x02, 0xa9, 0x01, 0xea]);
        let mut cpu = Cpu65816::default();
        cpu.step_with_bus(&mut bus, None);
        cpu.step_with_bus(&mut bus, None);
        assert_eq!(cpu.state().pc, 0x0005);
        cpu.step_with_bus(&mut bus, None);
        assert_eq!(cpu.state().a & 0xff, 0x00);
    }

    #[test]
    fn decimal_adc_uses_bcd_digits_and_carry() {
        let mut bus = FlatBus::new();
        bus.memory[..8].copy_from_slice(&[0xf8, 0x18, 0xa9, 0x09, 0x69, 0x01, 0xea, 0xea]);
        let mut cpu = Cpu65816::default();
        for _ in 0..5 {
            cpu.step_with_bus(&mut bus, None);
        }
        assert_eq!(cpu.state().a & 0xff, 0x10);
        assert_eq!(cpu.state().status & FLAG_CARRY, 0);
    }

    #[test]
    fn direct_and_absolute_load_families_use_the_data_bank() {
        let mut bus = FlatBus::new();
        bus.memory[0] = 0xa5;
        bus.memory[1] = 0x10;
        bus.memory[2] = 0xad;
        bus.memory[3] = 0x34;
        bus.memory[4] = 0x12;
        bus.memory[0x10] = 0x21;
        bus.memory[0x1234] = 0x43;
        let mut cpu = Cpu65816::default();
        cpu.step_with_bus(&mut bus, None);
        assert_eq!(cpu.state().a & 0xff, 0x21);
        cpu.step_with_bus(&mut bus, None);
        assert_eq!(cpu.state().a & 0xff, 0x43);
    }
}
