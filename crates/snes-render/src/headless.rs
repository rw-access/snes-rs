//! Headless execution API for ROM and golden-frame tests.

use std::path::Path;

use snes_core::{Console, LoadError, RomImage};

/// Per-ROM execution settings reserved for the test runner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RomTestDescriptor {
    pub mapping: Option<snes_core::MappingMode>,
    pub max_frames: Option<u64>,
    pub max_master_ticks: Option<u64>,
}

/// Headless runner boundary. Execution behavior is deferred to the console
/// milestone.
#[derive(Default)]
pub struct HeadlessRunner {
    descriptor: RomTestDescriptor,
}

impl HeadlessRunner {
    pub fn new(descriptor: RomTestDescriptor) -> Self {
        Self { descriptor }
    }

    pub fn descriptor(&self) -> &RomTestDescriptor {
        &self.descriptor
    }

    /// Loads a ROM image from bytes once cartridge loading is implemented.
    pub fn load_bytes(&self, bytes: Vec<u8>) -> Result<Console, LoadError> {
        Console::new(RomImage::new(bytes))
    }

    /// Reserved file-based entry point for the `snes-test` binary.
    pub fn load_path(&self, _path: &Path) -> Result<Console, LoadError> {
        Err(LoadError::UnsupportedFormat)
    }
}
