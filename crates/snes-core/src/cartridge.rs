//! Cartridge image and mapping extension points.

use crate::bus::{AccessSpeed, Address24};

/// Normalized cartridge metadata retained alongside the immutable image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RomMetadata {
    pub mapping: MappingMode,
    pub rom_size: usize,
    pub declared_rom_size: usize,
    pub sram_size: usize,
    pub region: u8,
    pub chipset: u8,
    pub title: [u8; 21],
    pub header_offset: Option<usize>,
    pub identity: u64,
}

/// Raw cartridge image supplied to the console.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RomImage {
    bytes: Vec<u8>,
    metadata: RomMetadata,
}

impl RomImage {
    /// Wraps ROM bytes without attempting to infer a mapping.
    pub fn new(bytes: Vec<u8>) -> Self {
        let identity = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
        });
        Self {
            metadata: RomMetadata {
                mapping: MappingMode::LoRom,
                rom_size: bytes.len(),
                declared_rom_size: bytes.len(),
                sram_size: 0,
                region: 0,
                chipset: 0,
                title: [b' '; 21],
                header_offset: None,
                identity,
            },
            bytes,
        }
    }

    /// Loads a raw or copier-headered image using its internal map-mode field.
    ///
    /// A cartridge header is data supplied by the ROM author, not a runtime
    /// requirement of the SNES.  In particular, enhancement-chip test ROMs
    /// commonly leave the title blank and use map modes 2/3/A, so title
    /// printability must not be used as a load gate.
    pub fn load(mut bytes: Vec<u8>) -> Result<Self, LoadError> {
        if bytes.is_empty() {
            return Err(LoadError::EmptyImage);
        }
        if bytes.len() % 1024 == 512 {
            bytes.drain(..512);
        }
        let candidates = [
            (0x7fc0, MappingMode::LoRom),
            (0xffc0, MappingMode::HiRom),
            // ExHiROM keeps the 64 KiB banking rules but stores its header in
            // the second 4 MiB window.  The baseline mapper mirrors the ROM
            // image, while retaining the header location for diagnostics.
            (0x40ffc0, MappingMode::HiRom),
        ];
        let (_, header_offset, mapping) = candidates
            .into_iter()
            .filter(|(offset, _)| *offset + 0x40 <= bytes.len())
            .filter_map(|(offset, mapping)| {
                let map_mode = bytes[offset + 0x15] & 0x0f;
                let valid = match offset {
                    0x7fc0 => matches!(map_mode, 0 | 2 | 3 | 0x0a),
                    0xffc0 => map_mode == 1,
                    0x40ffc0 => map_mode == 5,
                    _ => false,
                };
                if !valid {
                    return None;
                }
                let title_is_printable = bytes[offset..offset + 21]
                    .iter()
                    .all(|byte| (0x20..=0x7e).contains(byte));
                let reset_vector = u16::from_le_bytes([bytes[offset + 0x3c], bytes[offset + 0x3d]]);
                let confidence = u8::from(title_is_printable) * 2
                    + u8::from((0x8000..=0xffff).contains(&reset_vector));
                Some((confidence, offset, mapping))
            })
            .max_by_key(|(confidence, _, _)| *confidence)
            .ok_or(LoadError::UnsupportedFormat)?;
        let title = bytes[header_offset..header_offset + 21]
            .try_into()
            .map_err(|_| LoadError::UnsupportedFormat)?;
        let chipset = bytes[header_offset + 0x16];
        let declared_rom_size = 1024usize
            .checked_shl(u32::from(bytes[header_offset + 0x17]))
            .ok_or(LoadError::UnsupportedFormat)?;
        let sram_size = if bytes[header_offset + 0x18] == 0 {
            0
        } else {
            1024usize
                .checked_shl(u32::from(bytes[header_offset + 0x18]))
                .ok_or(LoadError::UnsupportedFormat)?
        };
        let mut image = Self::new(bytes);
        image.metadata.mapping = mapping;
        image.metadata.declared_rom_size = declared_rom_size;
        image.metadata.sram_size = sram_size;
        image.metadata.region = image.bytes[header_offset + 0x19];
        image.metadata.chipset = chipset;
        image.metadata.title = title;
        image.metadata.header_offset = Some(header_offset);
        Ok(image)
    }

    /// Overrides the mapping family selected for this image.
    ///
    /// The image bytes and all other loaded metadata remain unchanged. This
    /// is intended for callers such as test harnesses that have authoritative
    /// mapping information when a ROM header is absent or unreliable.
    pub fn with_mapping_override(mut self, mapping: MappingMode) -> Self {
        self.metadata.mapping = mapping;
        self
    }

    /// Returns the image bytes as provided by the caller.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns whether the image contains no bytes.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn metadata(&self) -> RomMetadata {
        self.metadata
    }

    pub fn identity(&self) -> u64 {
        self.metadata.identity
    }
}

/// Mapping families supported by the baseline cartridge loader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MappingMode {
    LoRom,
    HiRom,
}

/// Errors reserved for cartridge loading and validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadError {
    EmptyImage,
    UnsupportedFormat,
    UnsupportedCoprocessor,
}

/// Device boundary for cartridge mapping and future enhancement chips.
pub trait CartridgeDevice {
    fn read(&mut self, address: Address24) -> u8;
    fn write(&mut self, address: Address24, value: u8);
    fn access_speed(&self, address: Address24) -> AccessSpeed;

    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }

    fn restore_state(&mut self, _state: &[u8]) -> bool {
        true
    }
}

/// Placeholder cartridge device used until the LoROM/HiROM loader lands.
#[derive(Debug)]
pub(crate) struct RomOnlyCartridge {
    rom: RomImage,
    sram: Vec<u8>,
    enhancement_ram: Vec<u8>,
    enhancement_iram: Vec<u8>,
    enhancement_registers: Vec<u8>,
}

impl RomOnlyCartridge {
    pub(crate) fn new(rom: RomImage) -> Self {
        let chipset = rom.metadata().chipset & 0xf0;
        // Keep the cartridge-side RAM windows alive even before a dedicated
        // coprocessor core is present.  This matters for diagnostic ROMs that
        // validate the memory map independently of the enhancement CPU.
        let enhancement_ram_size = match chipset {
            0x10 => 0x10_000, // GSU/SuperFX game RAM window
            0x30 => 0x20_000, // SA-1 BW-RAM window
            0xf0 => 0x0c00,   // CX4 work RAM window
            _ => 0,
        };
        Self {
            sram: vec![0; rom.metadata().sram_size],
            enhancement_ram: vec![0; enhancement_ram_size],
            enhancement_iram: vec![0; if chipset == 0x30 { 0x800 } else { 0 }],
            enhancement_registers: vec![0; 0x400],
            rom,
        }
    }
}

impl CartridgeDevice for RomOnlyCartridge {
    fn read(&mut self, address: Address24) -> u8 {
        let raw = address.get() as usize;
        if let Some(index) = self.enhancement_index(raw) {
            return match index {
                EnhancementIndex::Ram(index) => self.enhancement_ram[index],
                EnhancementIndex::BwRam(index) => self.enhancement_ram[index],
                EnhancementIndex::IRam(index) => self.enhancement_iram[index],
                EnhancementIndex::Register(index) => self.read_enhancement_register(index),
                EnhancementIndex::Fixed(value) => value,
            };
        }
        if let Some(index) = self.sram_index(raw) {
            return self.sram[index];
        }
        let index = match self.rom.metadata().mapping {
            MappingMode::LoRom => ((raw & 0x7f_0000) >> 1) | (raw & 0x7fff),
            MappingMode::HiRom => (raw & 0x3f_0000) | (raw & 0xffff),
        } % self.rom.as_bytes().len();
        self.rom.as_bytes()[index]
    }

    fn write(&mut self, address: Address24, value: u8) {
        let raw = address.get() as usize;
        if let Some(index) = self.enhancement_index(raw) {
            match index {
                EnhancementIndex::Ram(index) => self.enhancement_ram[index] = value,
                EnhancementIndex::BwRam(index) => {
                    if self.sa1_bwram_write_allowed(index) {
                        self.enhancement_ram[index] = value;
                    }
                }
                EnhancementIndex::IRam(index) => {
                    if self.sa1_iram_write_allowed(index) {
                        self.enhancement_iram[index] = value;
                    }
                }
                EnhancementIndex::Register(index) => self.write_enhancement_register(index, value),
                EnhancementIndex::Fixed(_) => {}
            }
        } else if let Some(index) = self.sram_index(raw) {
            self.sram[index] = value;
        }
    }

    fn access_speed(&self, _address: Address24) -> AccessSpeed {
        AccessSpeed::Fast
    }

    fn save_state(&self) -> Vec<u8> {
        let mut state = self.sram.clone();
        state.extend_from_slice(&self.enhancement_ram);
        state.extend_from_slice(&self.enhancement_iram);
        state.extend_from_slice(&self.enhancement_registers);
        state
    }

    fn restore_state(&mut self, state: &[u8]) -> bool {
        let expected = self.sram.len()
            + self.enhancement_ram.len()
            + self.enhancement_iram.len()
            + self.enhancement_registers.len();
        if state.len() != expected {
            return false;
        }
        let (sram, state) = state.split_at(self.sram.len());
        self.sram.copy_from_slice(sram);
        let (ram, registers) = state.split_at(self.enhancement_ram.len());
        self.enhancement_ram.copy_from_slice(ram);
        let (iram, registers) = registers.split_at(self.enhancement_iram.len());
        self.enhancement_iram.copy_from_slice(iram);
        self.enhancement_registers.copy_from_slice(registers);
        true
    }
}

impl RomOnlyCartridge {
    fn enhancement_index(&self, raw: usize) -> Option<EnhancementIndex> {
        let chipset = self.rom.metadata().chipset & 0xf0;
        let bank = raw >> 16;
        let offset = raw & 0xffff;
        let system_bank = bank <= 0x3f || (0x80..=0xbf).contains(&bank);
        if !system_bank {
            return None;
        }
        match chipset {
            // SuperFX registers are exposed in the $3030-$32ff range.  The
            // register file is intentionally stateful so S-CPU firmware can
            // probe it while a full GSU execution core is added later.
            0x10 if (0x3000..=0x32ff).contains(&offset) => {
                Some(EnhancementIndex::Register(offset - 0x3000))
            }
            // SA-1 control/status registers are in the $2200-$23ff range.
            0x30 if (0x2200..=0x23ff).contains(&offset) => {
                Some(EnhancementIndex::Register(offset - 0x2200))
            }
            // SA-1 I-RAM is visible to the SNES CPU at $3000-$37ff.
            0x30 if (0x3000..=0x37ff).contains(&offset) => {
                Some(EnhancementIndex::IRam(offset - 0x3000))
            }
            // BMAPS selects the 8 KiB BW-RAM page visible to the SNES CPU.
            0x30 if (0x6000..=0x7fff).contains(&offset) => Some(EnhancementIndex::BwRam(
                self.sa1_bwram_index(offset - 0x6000, false),
            )),
            // CX4 exposes 3 KiB of RAM twice, with a hole between the mirrors,
            // followed by its register and vector windows.
            0xf0 if (0x6000..=0x6bff).contains(&offset) => {
                Some(EnhancementIndex::Ram(offset - 0x6000))
            }
            0xf0 if (0x7000..=0x7bff).contains(&offset) => {
                Some(EnhancementIndex::Ram(offset - 0x7000))
            }
            0xf0 if (0x6c00..=0x6fff).contains(&offset) || (0x7c00..=0x7f3f).contains(&offset) => {
                Some(EnhancementIndex::Fixed(0))
            }
            0xf0 if (0x7f40..=0x7f5f).contains(&offset) => {
                Some(EnhancementIndex::Register(offset - 0x7f40))
            }
            0xf0 if (0x7f60..=0x7f7f).contains(&offset) => {
                Some(EnhancementIndex::Register(offset - 0x7f40))
            }
            0xf0 if (0x7f80..=0x7faf).contains(&offset) => {
                Some(EnhancementIndex::Register(offset - 0x7f80 + 0x80))
            }
            0xf0 if (0x7fb0..=0x7fbf).contains(&offset) || (0x7ff0..=0x7fff).contains(&offset) => {
                Some(EnhancementIndex::Fixed(0))
            }
            0xf0 if (0x7fc0..=0x7fef).contains(&offset) => {
                Some(EnhancementIndex::Register(offset - 0x7fc0 + 0x80))
            }
            _ => None,
        }
    }

    fn sa1_bwram_index(&self, offset: usize, _sa1_cpu: bool) -> usize {
        let mapping = if _sa1_cpu {
            self.enhancement_registers[0x25] & 0x7f
        } else {
            self.enhancement_registers[0x24] & 0x1f
        };
        (usize::from(mapping) * 0x2000 + offset) % self.enhancement_ram.len()
    }

    fn sa1_bwram_write_allowed(&self, index: usize) -> bool {
        let enabled = self.enhancement_registers[0x26] & 0x80 != 0;
        let protected_pages = usize::from(self.enhancement_registers[0x28] & 0x0f).min(0x0a);
        let protected_size = 0x100usize << protected_pages;
        enabled || index >= protected_size
    }

    fn sa1_iram_write_allowed(&self, index: usize) -> bool {
        let mask = self.enhancement_registers[0x29];
        mask & (1 << (index / 0x100)) == 0
    }

    fn write_enhancement_register(&mut self, index: usize, value: u8) {
        match self.rom.metadata().chipset & 0xf0 {
            0x30 => self.write_sa1_register(index, value),
            0xf0 => self.write_cx4_register(index, value),
            _ => self.enhancement_registers[index] = value,
        }
    }

    fn read_enhancement_register(&self, index: usize) -> u8 {
        if self.rom.metadata().chipset & 0xf0 == 0x30 {
            match index {
                // The status/timer/math read ports start clear until a
                // coprocessor execution core supplies live values.
                0x100..=0x10e => self.enhancement_registers[index],
                _ => 0,
            }
        } else if self.rom.metadata().chipset & 0xf0 == 0xf0 {
            // The CX4 status port is mirrored at the neighboring read ports
            // used by existing games and by the hardware test ROM.
            match index {
                0x13..=0x17 | 0x19 | 0x1b..=0x1f => self.enhancement_registers[0x1e],
                _ => self.enhancement_registers[index],
            }
        } else {
            self.enhancement_registers[index]
        }
    }

    fn write_sa1_register(&mut self, index: usize, value: u8) {
        let value = match index {
            0x00 | 0x09 => value,
            0x24 => value & 0x1f,
            0x25 => value & 0xbf,
            0x26 | 0x27 => value & 0x80,
            0x28 => value & 0x0f,
            _ => value,
        };
        self.enhancement_registers[index] = value;
    }

    fn write_cx4_register(&mut self, index: usize, value: u8) {
        let mask = match index {
            0x08 => 0x01,        // $7f48
            0x0c => 0x03,        // $7f4c
            0x0e => 0x7f,        // $7f4e
            0x10 => 0x77,        // $7f50
            0x11 | 0x12 => 0x01, // $7f51-$7f52
            _ => 0xff,
        };
        self.enhancement_registers[index] = value & mask;
        if index == 0x0f {
            self.run_cx4_command(value);
        }
        if index == 0x1e {
            self.enhancement_registers[index] &= !0x01;
        }
    }

    fn run_cx4_command(&mut self, command: u8) {
        match command {
            // The diagnostic/test commands are synchronous from the cartridge
            // device's point of view; a future CX4 core can replace this with
            // a timed busy state without changing the mapping contract.
            0x25 => {
                let left = self.cx4_register_u24(0x80);
                let right = self.cx4_register_u24(0x83);
                let product =
                    Self::sign_extend_24(left).wrapping_mul(Self::sign_extend_24(right)) as u64;
                self.set_cx4_register_u24(0x80, product as u32 & 0x00ff_ffff);
                self.set_cx4_register_u24(0x83, (product >> 24) as u32 & 0x00ff_ffff);
            }
            0x40 => {
                let sum = self.enhancement_ram[..0x800]
                    .iter()
                    .fold(0u32, |sum, byte| sum.wrapping_add(u32::from(*byte)));
                self.set_cx4_register_u24(0x80, sum & 0x00ff_ffff);
            }
            0x54 => {
                let value = self.cx4_register_u24(0x80);
                let square = u64::from(value).wrapping_mul(u64::from(value));
                self.set_cx4_register_u24(0x83, square as u32 & 0x00ff_ffff);
                self.set_cx4_register_u24(0x86, (square >> 24) as u32 & 0x00ff_ffff);
            }
            _ => {}
        }
        self.enhancement_registers[0x1e] &= !0x40;
    }

    fn cx4_register_u24(&self, index: usize) -> u32 {
        u32::from(self.enhancement_registers[index])
            | (u32::from(self.enhancement_registers[index + 1]) << 8)
            | (u32::from(self.enhancement_registers[index + 2]) << 16)
    }

    fn set_cx4_register_u24(&mut self, index: usize, value: u32) {
        self.enhancement_registers[index] = value as u8;
        self.enhancement_registers[index + 1] = (value >> 8) as u8;
        self.enhancement_registers[index + 2] = (value >> 16) as u8;
    }

    fn sign_extend_24(value: u32) -> i64 {
        let value = value & 0x00ff_ffff;
        if value & 0x0080_0000 != 0 {
            i64::from(value) - (1_i64 << 24)
        } else {
            i64::from(value)
        }
    }

    fn sram_index(&self, raw: usize) -> Option<usize> {
        if self.sram.is_empty() {
            return None;
        }
        let bank = raw >> 16;
        let offset = raw & 0xffff;
        let byte = match self.rom.metadata().mapping {
            MappingMode::LoRom
                if (0x70..=0x7d).contains(&bank) || (0xf0..=0xff).contains(&bank) =>
            {
                (bank.saturating_sub(0x70)) * 0x8000 + offset
            }
            MappingMode::HiRom
                if (0x20..=0x3f).contains(&bank) || (0xa0..=0xbf).contains(&bank) =>
            {
                if !(0x6000..=0x7fff).contains(&offset) {
                    return None;
                }
                (bank & 0x1f) * 0x2000 + (offset - 0x6000)
            }
            _ => return None,
        };
        Some(byte % self.sram.len())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnhancementIndex {
    Ram(usize),
    BwRam(usize),
    IRam(usize),
    Register(usize),
    Fixed(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_detects_lorom_and_strips_copier_header() {
        let mut rom = vec![0; 0x8000];
        rom[0] = 0xa9;
        rom[0x7fc0..0x7fd5].fill(b' ');
        rom[0x7fc0..0x7fc5].copy_from_slice(b"TEST ");
        rom[0x7fd5] = 0x20;
        rom[0x7fd6] = 0x02;
        rom[0x7fd7] = 0x08;
        rom[0x7fd8] = 0x05;
        rom[0x7fd9] = 0x01;
        let mut headered = vec![0x55; 512];
        headered.extend_from_slice(&rom);

        let image = RomImage::load(headered).unwrap();
        assert_eq!(image.metadata().mapping, MappingMode::LoRom);
        assert_eq!(image.metadata().header_offset, Some(0x7fc0));
        assert_eq!(image.metadata().sram_size, 32 * 1024);
        assert_eq!(image.as_bytes()[0], 0xa9);
    }

    #[test]
    fn loader_detects_hirom_and_maps_linear_banks() {
        let mut rom = vec![0; 0x10000];
        rom[0] = 0x42;
        rom[0xffc0..0xffd5].fill(b' ');
        rom[0xffc0..0xffc5].copy_from_slice(b"HIROM");
        rom[0xffd5] = 0x21;
        rom[0xffd7] = 0x09;
        let image = RomImage::load(rom).unwrap();
        assert_eq!(image.metadata().mapping, MappingMode::HiRom);
        let mut cartridge = RomOnlyCartridge::new(image);
        assert_eq!(cartridge.read(Address24::new(0xc00000)), 0x42);
    }

    #[test]
    fn loader_accepts_blank_title_in_a_valid_hirom_header() {
        let mut rom = vec![0; 0x10000];
        rom[0xffd5] = 0x31;
        rom[0xffd7] = 0x06;
        rom[0xfffc] = 0x00;
        rom[0xfffd] = 0x80;

        let image = RomImage::load(rom).unwrap();

        assert_eq!(image.metadata().mapping, MappingMode::HiRom);
        assert_eq!(image.metadata().header_offset, Some(0xffc0));
    }

    #[test]
    fn loader_accepts_enhancement_chip_map_modes_and_metadata() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x23;
        rom[0x7fd6] = 0x35;
        rom[0x7fd7] = 0x08;

        let image = RomImage::load(rom).unwrap();

        assert_eq!(image.metadata().mapping, MappingMode::LoRom);
        assert_eq!(image.metadata().chipset, 0x35);
    }

    #[test]
    fn enhancement_cartridge_windows_are_stateful_and_snapshottable() {
        let mut cx4 = vec![0; 0x8000];
        cx4[0x7fd5] = 0x20;
        cx4[0x7fd6] = 0xf3;
        let mut cx4 = RomOnlyCartridge::new(RomImage::load(cx4).unwrap());
        cx4.write(Address24::new(0x006123), 0xa5);
        assert_eq!(cx4.read(Address24::new(0x006123)), 0xa5);

        let mut sa1 = vec![0; 0x8000];
        sa1[0x7fd5] = 0x23;
        sa1[0x7fd6] = 0x35;
        let mut sa1 = RomOnlyCartridge::new(RomImage::load(sa1).unwrap());
        sa1.write(Address24::new(0x002226), 0x80);
        sa1.write(Address24::new(0x006123), 0x5a);
        assert_eq!(sa1.read(Address24::new(0x006123)), 0x5a);
        assert_eq!(sa1.read(Address24::new(0x002220)), 0);
        let state = sa1.save_state();
        sa1.write(Address24::new(0x006123), 0);
        assert!(sa1.restore_state(&state));
        assert_eq!(sa1.read(Address24::new(0x006123)), 0x5a);
        assert_eq!(sa1.read(Address24::new(0x002220)), 0);
    }

    #[test]
    fn sa1_bwram_mapping_and_protection_are_cartridge_local() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x23;
        rom[0x7fd6] = 0x35;
        let mut cartridge = RomOnlyCartridge::new(RomImage::load(rom).unwrap());

        // Cold SA-1 BW-RAM is write-protected at the first 256 bytes.
        cartridge.write(Address24::new(0x006000), 0x11);
        assert_eq!(cartridge.read(Address24::new(0x006000)), 0);

        cartridge.write(Address24::new(0x002226), 0x80); // SBWE
        cartridge.write(Address24::new(0x002224), 0x01); // BMAPS page 1
        cartridge.write(Address24::new(0x006000), 0x22);
        assert_eq!(cartridge.read(Address24::new(0x006000)), 0x22);

        cartridge.write(Address24::new(0x002224), 0x00);
        assert_eq!(cartridge.read(Address24::new(0x006000)), 0);
        cartridge.write(Address24::new(0x002224), 0x01);
        assert_eq!(cartridge.read(Address24::new(0x006000)), 0x22);

        cartridge.write(Address24::new(0x002226), 0x00);
        cartridge.write(Address24::new(0x002228), 0x00); // protect first 256 bytes
        cartridge.write(Address24::new(0x002224), 0x00);
        cartridge.write(Address24::new(0x006000), 0x33);
        assert_eq!(cartridge.read(Address24::new(0x006000)), 0);
    }

    #[test]
    fn cx4_memory_aliases_register_masks_and_zero_windows_match_test_rom() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x20;
        rom[0x7fd6] = 0xf3;
        let mut cartridge = RomOnlyCartridge::new(RomImage::load(rom).unwrap());

        cartridge.write(Address24::new(0x006000), 0xa5);
        assert_eq!(cartridge.read(Address24::new(0x007000)), 0xa5);
        assert_eq!(cartridge.read(Address24::new(0x006c00)), 0);

        cartridge.write(Address24::new(0x007f48), 0xff);
        cartridge.write(Address24::new(0x007f4c), 0xff);
        cartridge.write(Address24::new(0x007f4e), 0xff);
        cartridge.write(Address24::new(0x007f50), 0xff);
        cartridge.write(Address24::new(0x007f51), 0xff);
        cartridge.write(Address24::new(0x007f52), 0xff);
        assert_eq!(cartridge.read(Address24::new(0x007f48)), 0x01);
        assert_eq!(cartridge.read(Address24::new(0x007f4c)), 0x03);
        assert_eq!(cartridge.read(Address24::new(0x007f4e)), 0x7f);
        assert_eq!(cartridge.read(Address24::new(0x007f50)), 0x77);
        assert_eq!(cartridge.read(Address24::new(0x007f51)), 0x01);
        assert_eq!(cartridge.read(Address24::new(0x007f52)), 0x01);

        cartridge.write(Address24::new(0x007f80), 0x5a);
        assert_eq!(cartridge.read(Address24::new(0x007fc0)), 0x5a);
        cartridge.write(Address24::new(0x007fb0), 0xff);
        assert_eq!(cartridge.read(Address24::new(0x007fb0)), 0);
    }

    #[test]
    fn cx4_simple_commands_complete_without_leaving_busy_status_set() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x20;
        rom[0x7fd6] = 0xf3;
        let mut cartridge = RomOnlyCartridge::new(RomImage::load(rom).unwrap());

        cartridge.write(Address24::new(0x007f80), 3);
        cartridge.write(Address24::new(0x007f81), 0);
        cartridge.write(Address24::new(0x007f82), 0);
        cartridge.write(Address24::new(0x007f83), 4);
        cartridge.write(Address24::new(0x007f84), 0);
        cartridge.write(Address24::new(0x007f85), 0);
        cartridge.write(Address24::new(0x007f4f), 0x25);

        assert_eq!(cartridge.read(Address24::new(0x007f80)), 12);
        assert_eq!(cartridge.read(Address24::new(0x007f5e)) & 0x40, 0);
    }

    #[test]
    fn mapping_override_replaces_detected_mapping_without_changing_image() {
        let mut rom = vec![0; 0x10000];
        rom[0xffc0..0xffd5].fill(b' ');
        rom[0xffd5] = 0x21;
        rom[0xffd7] = 0x09;
        let image = RomImage::load(rom).unwrap();
        let identity = image.identity();

        let overridden = image.with_mapping_override(MappingMode::LoRom);

        assert_eq!(overridden.metadata().mapping, MappingMode::LoRom);
        assert_eq!(overridden.metadata().header_offset, Some(0xffc0));
        assert_eq!(overridden.identity(), identity);
        assert_eq!(overridden.as_bytes().len(), 0x10000);
    }

    #[test]
    fn lorom_sram_window_is_writable_and_snapshottable() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fc0..0x7fd5].fill(b' ');
        rom[0x7fd5] = 0x20;
        rom[0x7fd7] = 0x08;
        rom[0x7fd8] = 0x05;
        let image = RomImage::load(rom).unwrap();
        let mut cartridge = RomOnlyCartridge::new(image);
        cartridge.write(Address24::new(0x700123), 0x5a);
        assert_eq!(cartridge.read(Address24::new(0x700123)), 0x5a);
        let state = cartridge.save_state();
        cartridge.write(Address24::new(0x700123), 0);
        assert!(cartridge.restore_state(&state));
        assert_eq!(cartridge.read(Address24::new(0x700123)), 0x5a);
    }
}
