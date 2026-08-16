//! A bounded, bus-independent core for the Super FX / GSU processor.
//!
//! The GSU has a deliberately small instruction set, but its instruction byte
//! is interpreted through the ALT1/ALT2/ALT3 mode bits.  This module keeps that
//! decoding and the register/flag semantics separate from cartridge mapping
//! and from the SNES scheduler.  A future console integration only needs to
//! implement [`GsuBus`], expose the GSU register window, and account for the
//! returned instruction cost.

/// The GSU has sixteen 16-bit registers.  R15 is the program counter.
pub const REGISTER_COUNT: usize = 16;

/// Bus used by the GSU core.  ROM and RAM are intentionally separate because
/// the GSU has different address/bank paths for them.
pub trait GsuBus {
    fn read_rom(&mut self, address: u32) -> u8;
    fn read_ram(&mut self, address: u32) -> u8;
    fn write_ram(&mut self, address: u32, value: u8);
}

/// A small flat memory implementation useful for unit tests and simple
/// frontends.  Addresses wrap at the end of each backing store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlatGsuMemory {
    pub rom: Vec<u8>,
    pub ram: Vec<u8>,
}

impl FlatGsuMemory {
    pub fn new(rom_size: usize, ram_size: usize) -> Self {
        Self {
            rom: vec![0; rom_size.max(1)],
            ram: vec![0; ram_size.max(1)],
        }
    }

    pub fn from_program(program: &[u8], ram_size: usize) -> Self {
        let mut memory = Self::new(program.len(), ram_size);
        memory.rom[..program.len()].copy_from_slice(program);
        memory
    }

    fn read_wrapped(bytes: &[u8], address: u32) -> u8 {
        bytes[address as usize % bytes.len()]
    }

    fn write_wrapped(bytes: &mut [u8], address: u32, value: u8) {
        let index = address as usize % bytes.len();
        bytes[index] = value;
    }
}

impl GsuBus for FlatGsuMemory {
    fn read_rom(&mut self, address: u32) -> u8 {
        Self::read_wrapped(&self.rom, address)
    }

    fn read_ram(&mut self, address: u32) -> u8 {
        Self::read_wrapped(&self.ram, address)
    }

    fn write_ram(&mut self, address: u32, value: u8) {
        Self::write_wrapped(&mut self.ram, address, value);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GsuFlags {
    pub sign: bool,
    pub zero: bool,
    pub overflow: bool,
    pub carry: bool,
}

impl GsuFlags {
    /// The four arithmetic bits in the Super FX status register.
    pub fn status_bits(self) -> u8 {
        // SFR's low byte is laid out as ... OV S CY Z (bits 4..1).  This is
        // distinct from the VSCZ display order used by the PeterLemon tests.
        let mut status = 0;
        if self.overflow {
            status |= 1 << 4;
        }
        if self.sign {
            status |= 1 << 3;
        }
        if self.carry {
            status |= 1 << 2;
        }
        if self.zero {
            status |= 1 << 1;
        }
        status
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GsuAlt {
    #[default]
    None,
    Alt1,
    Alt2,
    Alt3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GsuStep {
    pub pc: u16,
    pub opcode: u8,
    pub next_pc: u16,
    pub cycles: u32,
    pub halted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GsuError {
    UnsupportedOpcode { pc: u16, opcode: u8, alt: GsuAlt },
    StepLimit { limit: usize },
}

/// The executable state of the bounded GSU core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gsu {
    registers: [u16; REGISTER_COUNT],
    source: usize,
    destination: usize,
    move_mode: bool,
    flags: GsuFlags,
    alt: GsuAlt,
    halted: bool,
    stop_pending: bool,
    last_ram_address: u16,
    program_bank: u8,
    rom_bank: u8,
    ram_bank: u8,
    cache_base: u16,
    cache_valid: bool,
    cache: [u8; 512],
    color: u8,
}

impl Default for Gsu {
    fn default() -> Self {
        Self::new()
    }
}

impl Gsu {
    pub fn new() -> Self {
        Self {
            registers: [0; REGISTER_COUNT],
            source: 0,
            destination: 0,
            move_mode: false,
            flags: GsuFlags::default(),
            alt: GsuAlt::None,
            halted: true,
            stop_pending: false,
            last_ram_address: 0,
            program_bank: 0,
            rom_bank: 0,
            ram_bank: 0,
            cache_base: 0,
            cache_valid: false,
            cache: [0; 512],
            color: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn registers(&self) -> &[u16; REGISTER_COUNT] {
        &self.registers
    }

    pub fn register(&self, index: usize) -> u16 {
        assert!(index < REGISTER_COUNT, "GSU register index out of range");
        self.registers[index]
    }

    pub fn set_register(&mut self, index: usize, value: u16) {
        assert!(index < REGISTER_COUNT, "GSU register index out of range");
        self.registers[index] = value;
    }

    pub fn pc(&self) -> u16 {
        self.registers[15]
    }

    pub fn set_pc(&mut self, value: u16) {
        self.registers[15] = value;
    }

    pub fn flags(&self) -> GsuFlags {
        self.flags
    }

    pub fn clear_flags(&mut self) {
        self.flags = GsuFlags::default();
    }

    pub fn alt(&self) -> GsuAlt {
        self.alt
    }

    pub fn halted(&self) -> bool {
        self.halted
    }

    pub fn start(&mut self) {
        self.halted = false;
        self.source = 0;
        self.destination = 0;
        self.move_mode = false;
        self.alt = GsuAlt::None;
        self.stop_pending = false;
    }

    pub fn stop(&mut self) {
        self.halted = true;
    }

    pub fn program_bank(&self) -> u8 {
        self.program_bank
    }

    pub fn set_program_bank(&mut self, value: u8) {
        self.program_bank = value & 0x7f;
    }

    pub fn rom_bank(&self) -> u8 {
        self.rom_bank
    }

    pub fn set_rom_bank(&mut self, value: u8) {
        self.rom_bank = value & 0x7f;
        self.cache_valid = false;
    }

    pub fn ram_bank(&self) -> u8 {
        self.ram_bank
    }

    pub fn set_ram_bank(&mut self, value: u8) {
        self.ram_bank = value & 0x01;
    }

    pub fn color(&self) -> u8 {
        self.color
    }

    pub fn cache_base(&self) -> u16 {
        self.cache_base
    }

    pub fn set_cache_base(&mut self, value: u16) {
        self.cache_base = value & 0x01f0;
        self.cache_valid = false;
    }

    pub fn cache_valid(&self) -> bool {
        self.cache_valid
    }

    pub fn read_cache(&self, address: u16) -> u8 {
        self.cache[usize::from(address) & 0x01ff]
    }

    pub fn write_cache(&mut self, address: u16, value: u8) {
        self.cache[usize::from(address) & 0x01ff] = value;
    }

    pub fn cache_snapshot(&self) -> [u8; 512] {
        self.cache
    }

    /// Execute one instruction.  Unsupported rendering instructions return an
    /// error so the integration layer cannot accidentally claim full GSU
    /// compatibility.
    pub fn step<B: GsuBus>(&mut self, bus: &mut B) -> Result<GsuStep, GsuError> {
        let pc = self.pc();
        let opcode = self.fetch_rom(bus);
        let delay_slot = self.stop_pending;
        let cycles = self.execute(bus, pc, opcode)?;
        if delay_slot {
            self.stop_pending = false;
            self.halted = true;
        }
        Ok(GsuStep {
            pc,
            opcode,
            next_pc: self.pc(),
            cycles,
            halted: self.halted,
        })
    }

    /// Run until STOP or until `limit` instructions have been executed.
    pub fn run<B: GsuBus>(&mut self, bus: &mut B, limit: usize) -> Result<usize, GsuError> {
        self.start();
        for count in 0..limit {
            let step = self.step(bus)?;
            if step.opcode == 0 {
                return Ok(count + 1);
            }
            if self.halted {
                return Ok(count + 1);
            }
        }
        Err(GsuError::StepLimit { limit })
    }

    fn execute<B: GsuBus>(&mut self, bus: &mut B, pc: u16, opcode: u8) -> Result<u32, GsuError> {
        let n = (opcode & 0x0f) as usize;
        let cycles = match opcode {
            0x00 => {
                // STOP has a one-instruction delay slot.  The instruction
                // immediately following STOP is fetched and executed before
                // the GSU reports itself halted.
                self.stop_pending = true;
                1
            }
            0x01 => 1,
            0x02 => {
                self.cache_base = self.pc() & 0xfff0;
                self.cache_valid = true;
                1
            }
            0x03 => {
                let value = self.source_value() as u32;
                self.flags.carry = value & 1 != 0;
                self.write_result((value >> 1) as u16);
                1
            }
            0x04 => {
                let value = self.source_value();
                let carry = self.flags.carry;
                self.flags.carry = value & 0x8000 != 0;
                self.write_result(value.wrapping_shl(1) | carry as u16);
                1
            }
            0x05..=0x0f => {
                let offset = self.fetch_rom(bus) as i8 as i16;
                let take = match opcode {
                    0x05 => true,
                    0x06 => self.less_than_signed(),
                    0x07 => !self.less_than_signed(),
                    0x08 => !self.flags.zero,
                    0x09 => self.flags.zero,
                    0x0a => !self.flags.sign,
                    0x0b => self.flags.sign,
                    0x0c => !self.flags.carry,
                    0x0d => self.flags.carry,
                    0x0e => !self.flags.overflow,
                    0x0f => self.flags.overflow,
                    _ => false,
                };
                if take {
                    self.set_pc(self.pc().wrapping_add(offset as u16));
                }
                1
            }
            0x10..=0x1f => {
                if self.move_mode {
                    let value = self.source_value();
                    self.registers[n] = value;
                } else {
                    self.destination = n;
                }
                self.clear_alt();
                1
            }
            0x20..=0x2f => {
                self.source = n;
                self.destination = n;
                self.move_mode = true;
                self.clear_alt_mode_only();
                1
            }
            0x30..=0x3b => {
                let address = self.registers[n] as u32;
                self.last_ram_address = self.registers[n];
                self.write_word(bus, address, self.source_value());
                self.clear_alt();
                2
            }
            0x3c => {
                self.registers[12] = self.registers[12].wrapping_sub(1);
                self.flags.sign = self.registers[12] & 0x8000 != 0;
                self.flags.zero = self.registers[12] == 0;
                if !self.flags.zero {
                    self.set_pc(self.registers[13]);
                }
                self.clear_alt();
                1
            }
            0x3d => {
                self.alt = GsuAlt::Alt1;
                self.move_mode = false;
                1
            }
            0x3e => {
                self.alt = GsuAlt::Alt2;
                self.move_mode = false;
                1
            }
            0x3f => {
                self.alt = GsuAlt::Alt3;
                self.move_mode = false;
                1
            }
            0x40..=0x4b => {
                let address = self.registers[n] as u32;
                self.last_ram_address = self.registers[n];
                let value = self.read_word(bus, address);
                self.write_result(value);
                2
            }
            0x4c => {
                return Err(GsuError::UnsupportedOpcode {
                    pc,
                    opcode,
                    alt: self.alt,
                });
            }
            0x4d => {
                let value = self.source_value();
                self.write_result(value.rotate_left(8));
                1
            }
            0x4e => {
                if self.alt == GsuAlt::Alt1 {
                    return Err(GsuError::UnsupportedOpcode {
                        pc,
                        opcode,
                        alt: self.alt,
                    });
                }
                self.color = self.source_value() as u8;
                self.clear_alt();
                1
            }
            0x4f => {
                self.write_result(!self.source_value());
                1
            }
            0x50..=0x5f => {
                // ALT1 selects ADC Rn; ALT3 selects ADC #n.  WITH selects
                // the source and destination registers before ALT1 is seen,
                // so the opcode's nibbled register is only the RHS here.
                let immediate = matches!(self.alt, GsuAlt::Alt2 | GsuAlt::Alt3);
                let rhs = if immediate {
                    n as u16
                } else {
                    self.registers[n]
                };
                let with_carry = matches!(self.alt, GsuAlt::Alt1 | GsuAlt::Alt3);
                self.add(rhs, with_carry, true);
                1
            }
            0x60..=0x6f => {
                match self.alt {
                    GsuAlt::Alt1 => self.sub(self.registers[n], true, true),
                    GsuAlt::Alt2 => self.sub(n as u16, false, true),
                    GsuAlt::Alt3 => self.sub(self.registers[n], false, false),
                    GsuAlt::None => self.sub(self.registers[n], false, true),
                }
                1
            }
            0x70 => {
                let value = (self.registers[7] & 0xff00) | ((self.registers[8] & 0xff00) >> 8);
                // MERGE derives V/S/C/Z from selected bits of the merged
                // word, rather than from the usual arithmetic result.
                self.registers[self.destination] = value;
                self.flags.overflow = value & 0xc0c0 != 0;
                self.flags.sign = value & 0x8080 != 0;
                self.flags.carry = value & 0xe0e0 != 0;
                self.flags.zero = value & 0xf0f0 != 0;
                self.clear_alt();
                1
            }
            0x71..=0x7f => {
                let rhs = match self.alt {
                    GsuAlt::Alt1 => self.registers[n],
                    GsuAlt::Alt2 => n as u16,
                    GsuAlt::Alt3 => n as u16,
                    GsuAlt::None => self.registers[n],
                };
                let value = if matches!(self.alt, GsuAlt::Alt1 | GsuAlt::Alt3) {
                    self.source_value() & !rhs
                } else {
                    self.source_value() & rhs
                };
                self.write_result(value);
                1
            }
            0x80..=0x8f => {
                let rhs = match self.alt {
                    GsuAlt::Alt1 => self.registers[n] as u8 as u16,
                    GsuAlt::Alt2 => n as u16,
                    GsuAlt::Alt3 => n as u16,
                    GsuAlt::None => self.registers[n] as u8 as u16,
                };
                let lhs = self.source_value() as u8;
                let value = if matches!(self.alt, GsuAlt::Alt1 | GsuAlt::Alt3) {
                    (lhs as u16) * rhs
                } else {
                    (lhs as i8 as i32 * rhs as i8 as i32) as u16
                };
                self.write_result(value);
                2
            }
            0x90 => {
                self.write_word(bus, self.last_ram_address as u32, self.source_value());
                self.clear_alt();
                2
            }
            0x91..=0x94 => {
                self.registers[11] = self.pc().wrapping_add((opcode - 0x90) as u16);
                self.clear_alt();
                1
            }
            0x95 => {
                self.write_result(self.source_value() as i8 as i16 as u16);
                1
            }
            0x96 => {
                let value = self.source_value() as i16;
                self.flags.carry = value & 1 != 0;
                let shifted = if self.alt == GsuAlt::Alt1 && value == -1 {
                    0
                } else {
                    (value >> 1) as u16
                };
                self.write_result(shifted);
                1
            }
            0x97 => {
                let value = self.source_value();
                let carry = self.flags.carry;
                self.flags.carry = value & 1 != 0;
                self.write_result((value >> 1) | ((carry as u16) << 15));
                1
            }
            0x98..=0x9d => {
                let target = self.registers[n];
                if self.alt == GsuAlt::Alt1 {
                    self.program_bank = self.source_value() as u8 & 0x7f;
                    self.cache_valid = false;
                }
                self.set_pc(target);
                self.clear_alt();
                1
            }
            0x9e => {
                let source = self.source_value();
                let result = source & 0x00ff;
                self.write_result(result);
                self.flags.sign = result & 0x0080 != 0;
                1
            }
            0x9f => {
                let product = self.source_value() as i16 as i32 * self.registers[6] as i16 as i32;
                let upper = (product >> 16) as u16;
                if self.alt == GsuAlt::Alt1 {
                    self.registers[4] = product as u32 as u16;
                    self.flags.carry = self.registers[4] & 0x8000 != 0;
                } else {
                    self.flags.carry = (product as u32 & 0x8000) != 0;
                }
                self.write_result(upper);
                7
            }
            0xa0..=0xaf => match self.alt {
                GsuAlt::Alt2 => {
                    self.store_short(bus, n);
                    2
                }
                GsuAlt::Alt1 | GsuAlt::Alt3 => {
                    let address = self.fetch_rom(bus) as u16 * 2;
                    self.last_ram_address = address;
                    self.registers[n] = self.read_word(bus, address as u32);
                    self.clear_alt();
                    2
                }
                GsuAlt::None => {
                    let value = self.fetch_rom(bus) as i8 as i16 as u16;
                    self.registers[n] = value;
                    self.clear_alt();
                    1
                }
            },
            0xb0..=0xbf => {
                if self.move_mode {
                    let value = self.registers[n];
                    self.write_result(value);
                    self.flags.overflow = value & 0x8000 != 0;
                } else {
                    self.source = n;
                    self.clear_alt();
                }
                1
            }
            0xc0 => {
                let source = self.source_value();
                let result = source >> 8;
                self.write_result(result);
                self.flags.sign = result & 0x0080 != 0;
                1
            }
            0xc1..=0xcf => {
                let rhs = n as u16;
                let value = if matches!(self.alt, GsuAlt::Alt1 | GsuAlt::Alt3) {
                    self.source_value()
                        ^ if self.alt == GsuAlt::Alt3 {
                            rhs
                        } else {
                            self.registers[n]
                        }
                } else {
                    self.source_value()
                        | if self.alt == GsuAlt::Alt2 {
                            rhs
                        } else {
                            self.registers[n]
                        }
                };
                self.write_result(value);
                1
            }
            0xd0..=0xde => {
                self.registers[n] = self.registers[n].wrapping_add(1);
                self.flags.sign = self.registers[n] & 0x8000 != 0;
                self.flags.zero = self.registers[n] == 0;
                self.clear_alt();
                1
            }
            0xdf => match self.alt {
                GsuAlt::Alt1 => {
                    self.color = self.read_rom(self.rom_bank, self.registers[14], bus);
                    self.clear_alt();
                    1
                }
                GsuAlt::Alt2 => {
                    self.ram_bank = self.source_value() as u8 & 0x03;
                    self.clear_alt();
                    1
                }
                GsuAlt::Alt3 => {
                    self.rom_bank = self.source_value() as u8 & 0x7f;
                    self.clear_alt();
                    1
                }
                GsuAlt::None => {
                    self.color = self.read_rom(self.rom_bank, self.registers[14], bus);
                    self.clear_alt();
                    1
                }
            },
            0xe0..=0xee => {
                self.registers[n] = self.registers[n].wrapping_sub(1);
                self.flags.sign = self.registers[n] & 0x8000 != 0;
                self.flags.zero = self.registers[n] == 0;
                self.clear_alt();
                1
            }
            0xef => {
                let byte = self.read_rom(self.rom_bank, self.registers[14], bus);
                let source = self.source_value();
                let value = match self.alt {
                    GsuAlt::Alt1 => (source & 0x00ff) | ((byte as u16) << 8),
                    GsuAlt::Alt2 => (source & 0xff00) | byte as u16,
                    GsuAlt::Alt3 => byte as i8 as i16 as u16,
                    GsuAlt::None => byte as u16,
                };
                self.write_result(value);
                1
            }
            0xf0..=0xff => match self.alt {
                GsuAlt::Alt1 | GsuAlt::Alt3 => {
                    let address = self.fetch_rom(bus) as u16 | ((self.fetch_rom(bus) as u16) << 8);
                    self.last_ram_address = address;
                    self.registers[n] = self.read_word(bus, address as u32);
                    self.clear_alt();
                    2
                }
                GsuAlt::Alt2 => {
                    let address = self.fetch_rom(bus) as u16 | ((self.fetch_rom(bus) as u16) << 8);
                    self.last_ram_address = address;
                    self.write_word(bus, address as u32, self.registers[n]);
                    self.clear_alt();
                    2
                }
                GsuAlt::None => {
                    let value = self.fetch_rom(bus) as u16 | ((self.fetch_rom(bus) as u16) << 8);
                    self.registers[n] = value;
                    self.clear_alt();
                    2
                }
            },
        };
        Ok(cycles)
    }

    fn fetch_rom<B: GsuBus>(&mut self, bus: &mut B) -> u8 {
        let address = self.registers[15];
        let value = self.read_rom(self.program_bank, address, bus);
        self.registers[15] = address.wrapping_add(1);
        value
    }

    fn read_rom<B: GsuBus>(&self, bank: u8, address: u16, bus: &mut B) -> u8 {
        bus.read_rom(((bank as u32) << 16) | address as u32)
    }

    fn source_value(&self) -> u16 {
        self.registers[self.source]
    }

    fn write_result(&mut self, value: u16) {
        self.registers[self.destination] = value;
        self.flags.sign = value & 0x8000 != 0;
        self.flags.zero = value == 0;
        self.clear_alt();
    }

    fn clear_alt_mode_only(&mut self) {
        self.alt = GsuAlt::None;
    }

    fn clear_alt(&mut self) {
        self.alt = GsuAlt::None;
        self.move_mode = false;
    }

    fn less_than_signed(&self) -> bool {
        self.flags.sign != self.flags.overflow
    }

    fn add(&mut self, rhs: u16, with_carry: bool, write: bool) {
        let carry = if with_carry && self.flags.carry { 1 } else { 0 };
        let lhs = self.source_value();
        let wide = lhs as u32 + rhs as u32 + carry;
        let value = wide as u16;
        self.flags.carry = wide > 0xffff;
        self.flags.overflow = (!(lhs ^ rhs) & (lhs ^ value) & 0x8000) != 0;
        self.flags.sign = value & 0x8000 != 0;
        self.flags.zero = value == 0;
        if write {
            self.registers[self.destination] = value;
        }
        self.clear_alt();
    }

    fn sub(&mut self, rhs: u16, with_carry: bool, write: bool) {
        let borrow = if with_carry && !self.flags.carry {
            1
        } else {
            0
        };
        let lhs = self.source_value();
        let wide = lhs as i32 - rhs as i32 - borrow;
        let value = wide as u16;
        self.flags.carry = wide >= 0;
        self.flags.overflow = ((lhs ^ rhs) & (lhs ^ value) & 0x8000) != 0;
        self.flags.sign = value & 0x8000 != 0;
        self.flags.zero = value == 0;
        if write {
            self.registers[self.destination] = value;
        }
        self.clear_alt();
    }

    fn ram_address(&self, address: u16) -> u32 {
        ((self.ram_bank as u32) << 16) | address as u32
    }

    fn read_word<B: GsuBus>(&self, bus: &mut B, address: u32) -> u16 {
        let low = bus.read_ram(self.ram_address(address as u16));
        let high = bus.read_ram(self.ram_address((address as u16).wrapping_add(1)));
        u16::from_le_bytes([low, high])
    }

    fn write_word<B: GsuBus>(&self, bus: &mut B, address: u32, value: u16) {
        let [low, high] = value.to_le_bytes();
        bus.write_ram(self.ram_address(address as u16), low);
        bus.write_ram(self.ram_address((address as u16).wrapping_add(1)), high);
    }

    fn store_short<B: GsuBus>(&mut self, bus: &mut B, register: usize) {
        let address = self.fetch_rom(bus) as u16 * 2;
        self.last_ram_address = address;
        self.write_word(bus, address as u32, self.registers[register]);
        self.clear_alt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(program: &[u8]) -> (Gsu, FlatGsuMemory) {
        let mut gsu = Gsu::new();
        let mut memory = FlatGsuMemory::from_program(program, 0x10000);
        gsu.run(&mut memory, 128).expect("test program should stop");
        (gsu, memory)
    }

    #[test]
    fn immediate_add_sets_result_and_signed_overflow() {
        let (gsu, _) = run(&[0xf0, 0xff, 0x7f, 0x3e, 0x51, 0x00]);
        assert_eq!(gsu.register(0), 0x8000);
        assert!(gsu.flags().sign);
        assert!(gsu.flags().overflow);
        assert!(!gsu.flags().zero);
    }

    #[test]
    fn adc_register_form_matches_materialized_gsu_adc_case_zero() {
        let (gsu, _) = run(&[
            0xf1, 0xff, 0x7f, // iwt r1, $7fff
            0xf0, 0x01, 0x80, // iwt r0, $8001
            0x21, // with r1: source and destination are r1
            0x3d, // alt1: ADC register form
            0x50, // adc r0: $7fff + $8001 = $0000, with carry
            0x00,
        ]);

        assert_eq!(gsu.register(0), 0x8001);
        assert_eq!(gsu.register(1), 0x0000);
        assert_eq!(gsu.flags().status_bits(), 0x06); // VSCZ = 0011: C and Z
        assert!(!gsu.flags().overflow);
        assert!(!gsu.flags().sign);
        assert!(gsu.flags().carry);
        assert!(gsu.flags().zero);
    }

    #[test]
    fn register_selection_supports_gsu_move_form() {
        let (gsu, _) = run(&[
            0xf0, 0x34, 0x12, // iwt r0, $1234
            0xf1, 0xcd, 0xab, // iwt r1, $abcd
            0x21, // with r1: source and destination r1
            0x10, // to r0 while B is set: move r1 -> r0
            0x00,
        ]);
        assert_eq!(gsu.register(0), 0xabcd);
        assert_eq!(gsu.register(1), 0xabcd);
    }

    #[test]
    fn word_ram_transfers_are_little_endian_and_wrap() {
        let (gsu, memory) = run(&[
            0xf0, 0x34, 0x12, // iwt r0, $1234
            0xf1, 0xff, 0xff, // iwt r1, $ffff
            0x31, // stw (r1), source r0
            0x41, // ldw (r1), destination r0
            0x00,
        ]);
        assert_eq!(gsu.register(0), 0x1234);
        assert_eq!(memory.ram[0xffff], 0x34);
        assert_eq!(memory.ram[0], 0x12);
    }

    #[test]
    fn special_integer_operations_match_gsu_widths() {
        let (gsu, _) = run(&[
            0xa0, 0xfe, // ibt r0, -2
            0x95, // sex r0 => $fffe
            0x96, // asr r0 => $ffff
            0x4d, // swap r0 => $ffff
            0x9e, // lob r0 => $00ff
            0x00,
        ]);
        assert_eq!(gsu.register(0), 0x00ff);
    }

    #[test]
    fn compare_and_branch_do_not_modify_the_destination_register() {
        let (gsu, _) = run(&[
            0xf0, 1, 0, // iwt r0, 1
            0xf1, 1, 0,    // iwt r1, 1
            0x3f, // alt3
            0x61, // cmp r1
            0x09, 0x03, // beq over the failing immediate
            0xf0, 0xee, 0xee, 0x00,
        ]);
        assert_eq!(gsu.register(0), 1);
        assert!(gsu.flags().zero);
    }

    #[test]
    fn run_has_a_bounded_failure_mode() {
        let mut gsu = Gsu::new();
        let mut memory = FlatGsuMemory::from_program(&[0x01], 0x100);
        assert_eq!(
            gsu.run(&mut memory, 3),
            Err(GsuError::StepLimit { limit: 3 })
        );
    }
}
