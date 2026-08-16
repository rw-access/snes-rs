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

    /// Loads a raw or copier-headered image using its internal LoROM/HiROM
    /// header to select the ordinary cartridge mapping family.
    pub fn load(mut bytes: Vec<u8>) -> Result<Self, LoadError> {
        if bytes.is_empty() {
            return Err(LoadError::EmptyImage);
        }
        if bytes.len() % 1024 == 512 {
            bytes.drain(..512);
        }
        let candidates = [(0x7fc0, MappingMode::LoRom), (0xffc0, MappingMode::HiRom)];
        let (header_offset, mapping) = candidates
            .into_iter()
            .filter(|(offset, _)| *offset + 0x40 <= bytes.len())
            .filter_map(|(offset, mapping)| {
                let map_mode = bytes[offset + 0x15] & 0x0f;
                let expected = match mapping {
                    MappingMode::LoRom => 0,
                    MappingMode::HiRom => 1,
                };
                let title_is_printable = bytes[offset..offset + 21]
                    .iter()
                    .all(|byte| (0x20..=0x7e).contains(byte));
                (map_mode == expected && title_is_printable).then_some((offset, mapping))
            })
            .next()
            .ok_or(LoadError::UnsupportedFormat)?;
        let title = bytes[header_offset..header_offset + 21]
            .try_into()
            .map_err(|_| LoadError::UnsupportedFormat)?;
        let chipset = bytes[header_offset + 0x16];
        if chipset & 0x0f >= 3 {
            return Err(LoadError::UnsupportedCoprocessor);
        }
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
        let raw = address.get() as usize;
        let index = match self.rom.metadata().mapping {
            MappingMode::LoRom => ((raw & 0x7f_0000) >> 1) | (raw & 0x7fff),
            MappingMode::HiRom => (raw & 0x3f_0000) | (raw & 0xffff),
        } % self.rom.as_bytes().len();
        self.rom.as_bytes()[index]
    }

    fn write(&mut self, _address: Address24, _value: u8) {}

    fn access_speed(&self, _address: Address24) -> AccessSpeed {
        AccessSpeed::Fast
    }
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
}
