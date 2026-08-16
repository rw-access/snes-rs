//! Headless execution API for ROM and golden-frame tests.

use std::{fs, path::Path};

use snes_core::{Console, LoadError, RomImage};

/// Per-ROM execution settings reserved for the test runner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RomTestDescriptor {
    pub mapping: Option<snes_core::MappingMode>,
    pub max_frames: Option<u64>,
    pub max_master_ticks: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RomRunResult {
    pub master_ticks: u64,
    pub stopped: bool,
}

/// Headless runner for bounded, frontend-independent ROM execution.
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

    /// Loads a ROM image from bytes.
    pub fn load_bytes(&self, bytes: Vec<u8>) -> Result<Console, LoadError> {
        Console::new(RomImage::new(bytes))
    }

    /// Loads a ROM image from a file.
    pub fn load_path(&self, path: &Path) -> Result<Console, LoadError> {
        let bytes = fs::read(path).map_err(|_| LoadError::UnsupportedFormat)?;
        Console::new(RomImage::new(bytes))
    }

    pub fn run(&self, console: &mut Console) -> RomRunResult {
        console.reset();
        let limit = self.descriptor.max_master_ticks.unwrap_or(0);
        console.run_for_master_ticks(limit);
        RomRunResult {
            master_ticks: console.master_ticks(),
            stopped: console.is_stopped(),
        }
    }
}
