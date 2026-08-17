//! Cartridge image and mapping extension points.

use crate::{
    bus::{AccessSpeed, Address24},
    gsu::{Gsu, GsuBus},
};

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

    fn run_for_master_ticks(&mut self, _ticks: u64) {}

    fn read_sa1(&mut self, address: Address24) -> u8 {
        self.read(address)
    }

    fn write_sa1(&mut self, address: Address24, value: u8) {
        self.write(address, value);
    }

    fn sa1_reset_vector(&self) -> Option<u16> {
        None
    }

    fn take_sa1_irq(&mut self) -> bool {
        false
    }

    /// Returns and clears an interrupt raised for the SA-1 CPU by the S-CPU.
    fn take_sa1_cpu_irq(&mut self) -> bool {
        false
    }

    /// Returns and clears a reset request for the SA-1 CPU.
    fn take_sa1_reset(&mut self) -> bool {
        false
    }

    /// Returns and clears a ready request that releases an SA-1 stopped by STP.
    fn take_sa1_ready(&mut self) -> bool {
        false
    }

    /// Returns the vector used when the SA-1 raises an IRQ for the S-CPU.
    fn sa1_snes_irq_vector(&self) -> Option<u16> {
        None
    }

    /// Returns the vector used when the S-CPU raises an IRQ for the SA-1.
    fn sa1_cpu_irq_vector(&self) -> Option<u16> {
        None
    }

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
    gsu: Option<Gsu>,
    sa1_irq_pending: bool,
    sa1_cpu_irq_pending: bool,
    sa1_reset_pending: bool,
    sa1_ready_pending: bool,
    scpu_open_bus: u8,
    sa1_open_bus: u8,
}

impl RomOnlyCartridge {
    pub(crate) fn new(rom: RomImage) -> Self {
        let chipset = rom.metadata().chipset & 0xf0;
        // Keep the cartridge-side RAM windows alive even before a dedicated
        // coprocessor core is present.  This matters for diagnostic ROMs that
        // validate the memory map independently of the enhancement CPU.
        let enhancement_ram_size = match chipset {
            0x10 => 0x10_000, // GSU/SuperFX game RAM window
            0x30 => 0x4_0000, // SA-1 BW-RAM (256 KiB, mirrored in $40-$5f)
            0xf0 => 0x0c00,   // CX4 work RAM window
            _ => 0,
        };
        let mut enhancement_registers = vec![0; 0x400];
        if chipset == 0x30 {
            // SA-1 powers up with BWPA selecting the full addressable
            // BW-RAM range until software explicitly narrows it.
            enhancement_registers[0x28] = 0x0f;
        }
        Self {
            sram: vec![0; rom.metadata().sram_size],
            enhancement_ram: vec![0; enhancement_ram_size],
            enhancement_iram: vec![0; if chipset == 0x30 { 0x800 } else { 0 }],
            enhancement_registers,
            gsu: (chipset == 0x10).then(Gsu::new),
            sa1_irq_pending: false,
            sa1_cpu_irq_pending: false,
            sa1_reset_pending: false,
            sa1_ready_pending: false,
            scpu_open_bus: 0,
            sa1_open_bus: 0,
            rom,
        }
    }
}

impl CartridgeDevice for RomOnlyCartridge {
    fn read(&mut self, address: Address24) -> u8 {
        let mapped_enhancement = self.enhancement_index(address.get() as usize).is_some();
        let value = self.read_snes(address);
        if mapped_enhancement {
            self.scpu_open_bus = value;
        }
        value
    }

    fn write(&mut self, address: Address24, value: u8) {
        self.scpu_open_bus = value;
        self.write_snes(address, value);
    }

    fn read_sa1(&mut self, address: Address24) -> u8 {
        let raw = address.get() as usize;
        let mapped_enhancement = self.sa1_enhancement_index(raw).is_some();
        let value = if let Some(index) = self.sa1_enhancement_index(raw) {
            match index {
                EnhancementIndex::BwRam(index) | EnhancementIndex::Ram(index) => {
                    self.enhancement_ram[index]
                }
                EnhancementIndex::IRam(index) => self.enhancement_iram[index],
                EnhancementIndex::Register(index) => self.read_sa1_register(index),
                EnhancementIndex::Fixed(value) => value,
                EnhancementIndex::GsuCache(index) => self
                    .gsu
                    .as_ref()
                    .map_or(0, |gsu| gsu.read_cache(index as u16)),
            }
        } else if self.sa1_rom_mapped(raw) {
            self.read_snes(address)
        } else {
            0
        };
        // SA-1 instruction fetches from ROM do not replace the coprocessor's
        // I/O data-bus latch; the diagnostic relies on the latch seeded by
        // the preceding register write/read sequence.
        if mapped_enhancement {
            self.sa1_open_bus = value;
        }
        value
    }

    fn write_sa1(&mut self, address: Address24, value: u8) {
        let raw = address.get() as usize;
        self.sa1_open_bus = value;
        if let Some(index) = self.sa1_enhancement_index(raw) {
            match index {
                EnhancementIndex::BwRam(index) => {
                    self.sa1_open_bus = value;
                    if self.sa1_bwram_write_allowed(self.sa1_bwram_protection_index(raw, index)) {
                        self.enhancement_ram[index] = value;
                    }
                }
                EnhancementIndex::Ram(index) => self.enhancement_ram[index] = value,
                EnhancementIndex::IRam(index) => {
                    if self.sa1_iram_write_allowed(index, true) {
                        self.enhancement_iram[index] = value;
                    }
                }
                EnhancementIndex::Register(index) => {
                    self.write_sa1_register_from_sa1(index, value);
                }
                EnhancementIndex::Fixed(_) => {}
                EnhancementIndex::GsuCache(index) => {
                    if let Some(gsu) = self.gsu.as_mut() {
                        gsu.write_cache(index as u16, value);
                    }
                }
            }
        } else {
            self.write_snes(address, value);
        }
    }

    fn sa1_reset_vector(&self) -> Option<u16> {
        (self.rom.metadata().chipset & 0xf0 == 0x30)
            .then(|| {
                u16::from_le_bytes([
                    self.enhancement_registers[0x03],
                    self.enhancement_registers[0x04],
                ])
            })
            .filter(|vector| *vector != 0)
    }

    fn take_sa1_irq(&mut self) -> bool {
        let pending = self.sa1_irq_pending;
        self.sa1_irq_pending = false;
        pending
    }

    fn take_sa1_cpu_irq(&mut self) -> bool {
        let pending = self.sa1_cpu_irq_pending;
        self.sa1_cpu_irq_pending = false;
        pending
    }

    fn take_sa1_reset(&mut self) -> bool {
        let pending = self.sa1_reset_pending;
        self.sa1_reset_pending = false;
        pending
    }

    fn take_sa1_ready(&mut self) -> bool {
        let pending = self.sa1_ready_pending;
        self.sa1_ready_pending = false;
        pending
    }

    fn sa1_snes_irq_vector(&self) -> Option<u16> {
        if self.rom.metadata().chipset & 0xf0 != 0x30 {
            return None;
        }
        (self.enhancement_registers[0x09] & 0x40 != 0).then(|| {
            u16::from_le_bytes([
                self.enhancement_registers[0x0e],
                self.enhancement_registers[0x0f],
            ])
        })
    }

    fn sa1_cpu_irq_vector(&self) -> Option<u16> {
        if self.rom.metadata().chipset & 0xf0 != 0x30 {
            return None;
        }
        Some(u16::from_le_bytes([
            self.enhancement_registers[0x07],
            self.enhancement_registers[0x08],
        ]))
    }

    fn access_speed(&self, _address: Address24) -> AccessSpeed {
        AccessSpeed::Fast
    }

    fn run_for_master_ticks(&mut self, ticks: u64) {
        let Some(mut gsu) = self.gsu.take() else {
            return;
        };
        let count = (ticks / 6).saturating_add(1).min(100_000) as usize;
        let mut bus = CartridgeGsuBus {
            rom: self.rom.as_bytes(),
            ram: &mut self.enhancement_ram,
            cache: gsu.cache_snapshot(),
        };
        for _ in 0..count {
            if gsu.halted() {
                break;
            }
            if gsu.step(&mut bus).is_err() {
                break;
            }
        }
        self.gsu = Some(gsu);
    }

    fn save_state(&self) -> Vec<u8> {
        let mut state = self.sram.clone();
        state.extend_from_slice(&self.enhancement_ram);
        state.extend_from_slice(&self.enhancement_iram);
        state.extend_from_slice(&self.enhancement_registers);
        state.push(self.scpu_open_bus);
        state.push(self.sa1_open_bus);
        state
    }

    fn restore_state(&mut self, state: &[u8]) -> bool {
        let memory_state_len = self.sram.len()
            + self.enhancement_ram.len()
            + self.enhancement_iram.len()
            + self.enhancement_registers.len();
        if state.len() != memory_state_len && state.len() != memory_state_len + 2 {
            return false;
        }
        let (state, latches) = state.split_at(memory_state_len);
        let (sram, state) = state.split_at(self.sram.len());
        self.sram.copy_from_slice(sram);
        let (ram, registers) = state.split_at(self.enhancement_ram.len());
        self.enhancement_ram.copy_from_slice(ram);
        let (iram, registers) = registers.split_at(self.enhancement_iram.len());
        self.enhancement_iram.copy_from_slice(iram);
        self.enhancement_registers.copy_from_slice(registers);
        if latches.len() == 2 {
            self.scpu_open_bus = latches[0];
            self.sa1_open_bus = latches[1];
        } else {
            self.scpu_open_bus = 0;
            self.sa1_open_bus = 0;
        }
        true
    }
}

impl RomOnlyCartridge {
    fn read_snes(&mut self, address: Address24) -> u8 {
        let raw = address.get() as usize;
        let bank = raw >> 16;
        let offset = raw & 0xffff;
        if self.rom.metadata().chipset & 0xf0 == 0x30
            && (bank <= 0x3f || (0x80..=0xbf).contains(&bank))
            && offset == 0xffee
            && self.enhancement_registers[0x09] & 0x40 != 0
        {
            // With the SA-1 SNES IRQ vector override enabled, the ROM's
            // native IRQ-vector low byte is the message byte supplied in
            // SIVL.  SA-1 diagnostics read this address after entry.
            return self.enhancement_registers[0x0e];
        }
        if let Some(index) = self.enhancement_index(raw) {
            return match index {
                EnhancementIndex::Ram(index) => self.enhancement_ram[index],
                EnhancementIndex::BwRam(index) => self.enhancement_ram[index],
                EnhancementIndex::IRam(index) => self.enhancement_iram[index],
                EnhancementIndex::GsuCache(index) => self
                    .gsu
                    .as_ref()
                    .map_or(0, |gsu| gsu.read_cache(index as u16)),
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

    fn write_snes(&mut self, address: Address24, value: u8) {
        let raw = address.get() as usize;
        if let Some(index) = self.enhancement_index(raw) {
            match index {
                EnhancementIndex::Ram(index) => self.enhancement_ram[index] = value,
                EnhancementIndex::BwRam(index) => {
                    if self.sa1_bwram_write_allowed(self.sa1_bwram_protection_index(raw, index)) {
                        self.enhancement_ram[index] = value;
                    }
                }
                EnhancementIndex::IRam(index) => {
                    if self.sa1_iram_write_allowed(index, false) {
                        self.enhancement_iram[index] = value;
                    }
                }
                EnhancementIndex::GsuCache(index) => {
                    if let Some(gsu) = self.gsu.as_mut() {
                        gsu.write_cache(index as u16, value);
                    }
                }
                EnhancementIndex::Register(index) => self.write_enhancement_register(index, value),
                EnhancementIndex::Fixed(_) => {}
            }
        } else if let Some(index) = self.sram_index(raw) {
            self.sram[index] = value;
        }
    }

    fn enhancement_index(&self, raw: usize) -> Option<EnhancementIndex> {
        let chipset = self.rom.metadata().chipset & 0xf0;
        let bank = raw >> 16;
        let offset = raw & 0xffff;
        let system_bank = bank <= 0x3f || (0x80..=0xbf).contains(&bank);
        if chipset == 0x30 && (0x40..=0x4f).contains(&bank) {
            // The S-CPU's direct BW-RAM window decodes 256 KiB and mirrors
            // it through banks $40-$4f.  The SA-1 has a separate, larger
            // direct window handled by sa1_enhancement_index below.
            return Some(EnhancementIndex::BwRam(
                (((bank - 0x40) & 0x03) * 0x10000 + offset) % self.enhancement_ram.len(),
            ));
        }
        if !system_bank {
            return None;
        }
        match chipset {
            // SuperFX registers are exposed in the $3030-$32ff range.  The
            // register file is intentionally stateful so S-CPU firmware can
            // probe it while a full GSU execution core is added later.
            0x10 if (0x3000..=0x32ff).contains(&offset) => {
                if (0x3100..=0x32ff).contains(&offset) {
                    Some(EnhancementIndex::GsuCache(offset - 0x3100))
                } else {
                    Some(EnhancementIndex::Register(offset - 0x3000))
                }
            }
            // SA-1 control/status registers are in the $2200-$23ff range.
            0x30 if (0x2200..=0x23ff).contains(&offset) => {
                Some(EnhancementIndex::Register(offset - 0x2200))
            }
            // SA-1 I-RAM is visible to the SNES CPU at $3000-$37ff.
            0x30 if (0x3000..=0x37ff).contains(&offset) => {
                Some(EnhancementIndex::IRam((offset - 0x3000) & 0x07ff))
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

    fn sa1_enhancement_index(&self, raw: usize) -> Option<EnhancementIndex> {
        if self.rom.metadata().chipset & 0xf0 != 0x30 {
            return None;
        }
        let bank = raw >> 16;
        let offset = raw & 0xffff;
        let system_bank = bank <= 0x3f || (0x80..=0xbf).contains(&bank);
        if system_bank && (0x2200..=0x23ff).contains(&offset) {
            return Some(EnhancementIndex::Register(offset - 0x2200));
        }
        if system_bank && ((offset < 0x0800) || (0x3000..0x3800).contains(&offset)) {
            return Some(EnhancementIndex::IRam(offset & 0x07ff));
        }
        if (0x40..=0x5f).contains(&bank) {
            return Some(EnhancementIndex::BwRam(
                ((bank - 0x40) * 0x10000 + offset) % self.enhancement_ram.len(),
            ));
        }
        if system_bank && (0x6000..=0x7fff).contains(&offset) {
            return Some(EnhancementIndex::BwRam(
                self.sa1_bwram_index(offset - 0x6000, true),
            ));
        }
        None
    }

    fn sa1_rom_mapped(&self, raw: usize) -> bool {
        let bank = raw >> 16;
        let offset = raw & 0xffff;
        (bank <= 0x3f || (0x80..=0xbf).contains(&bank)) && offset >= 0x8000 || bank >= 0xc0
    }

    fn sa1_bwram_index(&self, offset: usize, _sa1_cpu: bool) -> usize {
        let mapping = if _sa1_cpu {
            self.enhancement_registers[0x25] & 0x7f
        } else {
            self.enhancement_registers[0x24] & 0x1f
        };
        (usize::from(mapping) * 0x2000 + offset) % self.enhancement_ram.len()
    }

    fn sa1_bwram_protection_index(&self, raw: usize, mapped_index: usize) -> usize {
        let bank = raw >> 16;
        if (0x40..=0x5f).contains(&bank) {
            // The protection comparator tops out at $43ffff even though the
            // address decoder mirrors the BW-RAM window into later banks.
            ((bank - 0x40) * 0x10000 + (raw & 0xffff)).min(0x3ffff)
        } else {
            mapped_index
        }
    }

    fn sa1_bwram_write_allowed(&self, index: usize) -> bool {
        let enabled = self.enhancement_registers[0x26] & 0x80 != 0
            || self.enhancement_registers[0x27] & 0x80 != 0;
        let protected_pages = usize::from(self.enhancement_registers[0x28] & 0x0f).min(0x0a);
        let protected_size = 0x100usize << protected_pages;
        enabled || index >= protected_size
    }

    fn sa1_iram_write_allowed(&self, index: usize, sa1_cpu: bool) -> bool {
        let mask = self.enhancement_registers[if sa1_cpu { 0x2a } else { 0x29 }];
        mask & (1 << (index / 0x100)) != 0
    }

    fn write_enhancement_register(&mut self, index: usize, value: u8) {
        match self.rom.metadata().chipset & 0xf0 {
            0x10 if index == 0x30 => {
                self.enhancement_registers[index] = value;
                if let Some(gsu) = self.gsu.as_mut() {
                    if value & 0x1e == 0 {
                        gsu.clear_flags();
                    }
                    if value & 0x20 != 0 {
                        gsu.start();
                    } else {
                        gsu.stop();
                    }
                }
            }
            0x10 if index < 0x20 => {
                if let Some(gsu) = self.gsu.as_mut() {
                    let register = index / 2;
                    let old = gsu.register(register);
                    let updated = if index & 1 == 0 {
                        (old & 0xff00) | u16::from(value)
                    } else {
                        (old & 0x00ff) | (u16::from(value) << 8)
                    };
                    gsu.set_register(register, updated);
                    if register == 15 && index & 1 != 0 {
                        gsu.set_pc(updated);
                        gsu.start();
                    }
                }
            }
            0x10 if index == 0x34 => {
                self.enhancement_registers[index] = value;
                if let Some(gsu) = self.gsu.as_mut() {
                    gsu.set_program_bank(value);
                }
            }
            0x10 if index == 0x36 => {
                self.enhancement_registers[index] = value;
                if let Some(gsu) = self.gsu.as_mut() {
                    gsu.set_rom_bank(value);
                }
            }
            0x10 if index == 0x3c => {
                self.enhancement_registers[index] = value & 0x01;
                if let Some(gsu) = self.gsu.as_mut() {
                    gsu.set_ram_bank(value);
                }
            }
            0x10 if (0x3e..=0x3f).contains(&index) => {
                self.enhancement_registers[index] = value;
                if let Some(gsu) = self.gsu.as_mut() {
                    let current = gsu.cache_base();
                    let updated = if index == 0x3e {
                        (current & 0xff00) | u16::from(value)
                    } else {
                        (current & 0x00ff) | (u16::from(value) << 8)
                    };
                    gsu.set_cache_base(updated);
                }
            }
            0x30 => {
                // These controls belong to the SA-1 side and are not
                // writable through the S-CPU register view.
                if matches!(index, 0x25 | 0x27 | 0x2a) {
                    return;
                }
                self.write_sa1_register(index, value);
                // CCNT bit 7 is the S-CPU-to-SA-1 IRQ request.  Keep this
                // edge in the cartridge so the second CPU can service it at
                // its next scheduling point.
                if index == 0x00 && value & 0x80 != 0 {
                    self.sa1_cpu_irq_pending = true;
                }
                if index == 0x00 && value & 0x20 != 0 {
                    self.sa1_reset_pending = true;
                    self.enhancement_registers[0x2a] = 0;
                }
                if index == 0x00 && value & 0x40 != 0 {
                    self.sa1_ready_pending = true;
                }
            }
            0xf0 => self.write_cx4_register(index, value),
            _ => self.enhancement_registers[index] = value,
        }
    }

    fn read_enhancement_register(&mut self, index: usize) -> u8 {
        if self.rom.metadata().chipset & 0xf0 == 0x10 {
            match index {
                0x00..=0x1f => {
                    let register = index / 2;
                    let value = self.gsu.as_ref().map_or(0, |gsu| gsu.register(register));
                    if index & 1 == 0 {
                        value as u8
                    } else {
                        (value >> 8) as u8
                    }
                }
                0x30 => self.gsu_status(),
                _ => self.enhancement_registers[index],
            }
        } else if self.rom.metadata().chipset & 0xf0 == 0x30 {
            match index {
                // The control/vector registers at $2200-$2215 are visible
                // through the S-CPU register window.  Keep these separate
                // from the $2300 status/read ports below; returning the
                // status-port default for the low indexes breaks the SA-1
                // message and reboot handshakes that poll these registers.
                0x00..=0x0f => self.scpu_open_bus,
                // SFR is the S-CPU's read view of the SA-1 control/message
                // flag (SCNT), while CFR is the SA-1's read view of the
                // S-CPU control/message flag (CCNT).
                0x100 => self.enhancement_registers[0x09],
                // The remaining ports are either SA-1-owned or undefined
                // from the S-CPU bus.  They retain the last S-CPU bus value.
                0x101..=0x10d | 0x10f..=0x110 => self.scpu_open_bus,
                // VC reports the SA-1 device revision used by the RF5A123.
                0x10e => 0x23,
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

    fn gsu_status(&self) -> u8 {
        let Some(gsu) = self.gsu.as_ref() else {
            return 0;
        };
        let mut status = gsu.flags().status_bits();
        if !gsu.halted() {
            status |= 0x20;
        }
        status
    }

    fn write_sa1_register(&mut self, index: usize, value: u8) {
        let value = match index {
            0x00 | 0x09 => value,
            0x24 => value & 0x1f,
            0x25 => value & 0x7f,
            0x26 | 0x27 => value & 0x80,
            0x28 => value & 0x0f,
            _ => value,
        };
        self.enhancement_registers[index] = value;
        if index == 0x01 && value & 0x80 != 0 && self.enhancement_registers[0x09] & 0x80 != 0 {
            self.sa1_irq_pending = true;
        }
    }

    fn read_sa1_register(&self, index: usize) -> u8 {
        match index {
            // SFR is S-CPU-only; the SA-1 sees its own bus latch here.
            0x100 => self.sa1_open_bus,
            0x101 => self.enhancement_registers[0x00],
            0x102..=0x10d => self.enhancement_registers[index],
            0x10e => 0x23,
            _ => self.sa1_open_bus,
        }
    }

    fn write_sa1_register_from_sa1(&mut self, index: usize, value: u8) {
        // The complementary controls are S-CPU-only.  The hardware ignores
        // an attempt by the SA-1 core to write them rather than sharing the
        // register storage between the two bus masters.
        if matches!(index, 0x01 | 0x24 | 0x26 | 0x28 | 0x29) {
            return;
        }
        self.write_sa1_register(index, value);
        if index == 0x09 && value & 0x80 != 0 && self.enhancement_registers[0x01] & 0x80 != 0 {
            self.sa1_irq_pending = true;
        }
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

struct CartridgeGsuBus<'a> {
    rom: &'a [u8],
    ram: &'a mut [u8],
    cache: [u8; 512],
}

impl GsuBus for CartridgeGsuBus<'_> {
    fn read_rom(&mut self, address: u32) -> u8 {
        let raw = address as usize;
        if raw < 0x0200 {
            return self.cache[raw & 0x01ff];
        }
        let index = (((raw & 0x7f_0000) >> 1) | (raw & 0x7fff)) % self.rom.len();
        self.rom[index]
    }

    fn read_ram(&mut self, address: u32) -> u8 {
        self.ram[address as usize % self.ram.len()]
    }

    fn write_ram(&mut self, address: u32, value: u8) {
        let index = address as usize % self.ram.len();
        self.ram[index] = value;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EnhancementIndex {
    Ram(usize),
    BwRam(usize),
    IRam(usize),
    GsuCache(usize),
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
    fn sa1_iram_write_protection_uses_the_snes_view_only_for_snes_accesses() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x23;
        rom[0x7fd6] = 0x35;
        let mut cartridge = RomOnlyCartridge::new(RomImage::load(rom).unwrap());

        cartridge.write(Address24::new(0x002229), 0x00);
        cartridge.write(Address24::new(0x003000), 0xaa);
        assert_eq!(cartridge.read(Address24::new(0x003000)), 0);

        cartridge.write(Address24::new(0x002229), 0x01);
        cartridge.write(Address24::new(0x003000), 0x55);
        assert_eq!(cartridge.read(Address24::new(0x003000)), 0x55);

        cartridge.write_sa1(Address24::new(0x00222a), 0x00);
        cartridge.write_sa1(Address24::new(0x000000), 0xcc);
        assert_eq!(cartridge.read_sa1(Address24::new(0x000000)), 0x55);
    }

    #[test]
    fn snes_iram_writes_are_visible_through_sa1_mirrors() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x23;
        rom[0x7fd6] = 0x35;
        let mut cartridge = RomOnlyCartridge::new(RomImage::load(rom).unwrap());
        cartridge.write(Address24::new(0x002229), 0xff);
        cartridge.write(Address24::new(0x003000), 0xaa);
        for address in [0x003000, 0x3f3000, 0x803000, 0xbf3000] {
            assert_eq!(
                cartridge.read(Address24::new(address)),
                0xaa,
                "${address:06x}"
            );
        }
        assert_eq!(cartridge.read_sa1(Address24::new(0x000000)), 0xaa);
        assert_eq!(cartridge.read_sa1(Address24::new(0x3f0000)), 0xaa);
        assert_eq!(cartridge.read_sa1(Address24::new(0x800000)), 0xaa);
        assert_eq!(cartridge.read_sa1(Address24::new(0xbf0000)), 0xaa);
    }

    #[test]
    fn sa1_register_read_ports_have_cpu_specific_open_bus_ownership() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x23;
        rom[0x7fd6] = 0x35;
        let mut cartridge = RomOnlyCartridge::new(RomImage::load(rom).unwrap());

        // S-CPU status is readable, but the neighboring SA-1/undefined
        // ports retain the S-CPU bus value. VC is the one fixed read port.
        cartridge.write(Address24::new(0x002300), 0);
        assert_eq!(cartridge.read(Address24::new(0x002300)), 0);
        assert_eq!(cartridge.read(Address24::new(0x002301)), 0);
        assert_eq!(cartridge.read(Address24::new(0x00230d)), 0);
        assert_eq!(cartridge.read(Address24::new(0x00230e)), 0x23);
        assert_eq!(cartridge.read(Address24::new(0x00230f)), 0x23);
        assert_eq!(cartridge.read(Address24::new(0x002310)), 0x23);

        // The SA-1 has its own latch. Its $2300 view is S-CPU-only, while
        // $2301 is the SA-1's CCNT status port.
        cartridge.write_sa1(Address24::new(0x002300), 0x5a);
        assert_eq!(cartridge.read_sa1(Address24::new(0x002300)), 0x5a);
        cartridge.write(Address24::new(0x002200), 0xa5);
        assert_eq!(cartridge.read_sa1(Address24::new(0x002301)), 0xa5);
        assert_eq!(cartridge.read_sa1(Address24::new(0x00230e)), 0x23);
        assert_eq!(cartridge.read_sa1(Address24::new(0x00230f)), 0x23);

        // An SA-1 ROM fetch must not change the S-CPU latch.
        cartridge.write(Address24::new(0x002301), 0x66);
        let _ = cartridge.read_sa1(Address24::new(0x008000));
        assert_eq!(cartridge.read(Address24::new(0x002301)), 0x66);
    }

    #[test]
    fn sa1_register_open_bus_latches_round_trip_through_state() {
        let mut rom = vec![0; 0x8000];
        rom[0x7fd5] = 0x23;
        rom[0x7fd6] = 0x35;
        let mut cartridge = RomOnlyCartridge::new(RomImage::load(rom).unwrap());

        cartridge.write(Address24::new(0x002301), 0x12);
        cartridge.write_sa1(Address24::new(0x00230f), 0x34);
        let state = cartridge.save_state();

        cartridge.write(Address24::new(0x002301), 0xa5);
        cartridge.write_sa1(Address24::new(0x00230f), 0x5a);
        assert!(cartridge.restore_state(&state));
        assert_eq!(cartridge.read(Address24::new(0x002301)), 0x12);
        assert_eq!(cartridge.read_sa1(Address24::new(0x00230f)), 0x34);
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
