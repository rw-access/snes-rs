//! Cartridge image and mapping extension points.

use crate::bus::{AccessSpeed, Address24};

/// Normalized cartridge metadata retained alongside the immutable image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RomMetadata {
    pub mapping: MappingMode,
    pub rom_size: usize,
    pub sram_size: usize,
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
                sram_size: 0,
                identity,
            },
            bytes,
        }
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
}

/// Device boundary for cartridge mapping and future enhancement chips.
pub trait CartridgeDevice {
    fn read(&mut self, address: Address24) -> u8;
    fn write(&mut self, address: Address24, value: u8);
    fn access_speed(&self, address: Address24) -> AccessSpeed;
}

/// Placeholder cartridge device used until the LoROM/HiROM loader lands.
#[derive(Debug)]
pub(crate) struct RomOnlyCartridge {
    rom: RomImage,
}

impl RomOnlyCartridge {
    pub(crate) fn new(rom: RomImage) -> Self {
        Self { rom }
    }
}

impl CartridgeDevice for RomOnlyCartridge {
    fn read(&mut self, address: Address24) -> u8 {
        self.rom
            .as_bytes()
            .get(address.get() as usize % self.rom.as_bytes().len())
            .copied()
            .unwrap_or(0)
    }

    fn write(&mut self, _address: Address24, _value: u8) {}

    fn access_speed(&self, _address: Address24) -> AccessSpeed {
        AccessSpeed::Fast
    }
}
