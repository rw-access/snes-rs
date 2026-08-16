//! Headless execution API for ROM and golden-frame tests.

use std::{fs, path::Path};

use snes_core::{Address24, Console, LoadError, RomImage, MASTER_TICKS_PER_FRAME};

/// A result convention explicitly described by one of the pinned SNES test
/// sources.  SNES test ROMs do not share the NES/Blargg signature, so result
/// extraction must be selected by an explicit descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResultProtocol {
    /// Read one byte from the 24-bit CPU address space. `running` means that
    /// the test is still in progress; `passed` is the only passing value and
    /// every other value is reported as a failure code.
    ///
    /// The pinned absindx SA-1 test documents this protocol at `$000000` with
    /// values 0 (running), 1 (passed), and 255 (failed).
    MemoryByte {
        address: Address24,
        running: u8,
        passed: u8,
    },
}

impl ResultProtocol {
    pub const fn memory_byte(address: Address24, running: u8, passed: u8) -> Self {
        Self::MemoryByte {
            address,
            running,
            passed,
        }
    }

    pub fn observe(&self, console: &Console) -> ResultObservation {
        match self {
            Self::MemoryByte {
                address,
                running,
                passed,
            } => classify(*address, console.read_memory(*address), *running, *passed),
        }
    }

    /// Applies the protocol to a memory reader without requiring a running
    /// console. This keeps descriptor tests independent of ROM files while
    /// using exactly the same classification as the headless runner.
    pub fn observe_memory(&self, read: impl Fn(Address24) -> u8) -> ResultObservation {
        match self {
            Self::MemoryByte {
                address,
                running,
                passed,
            } => classify(*address, read(*address), *running, *passed),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultStatus {
    Running,
    Passed,
    Failed(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultObservation {
    pub address: Address24,
    pub value: u8,
    pub status: ResultStatus,
}

fn classify(address: Address24, value: u8, running: u8, passed: u8) -> ResultObservation {
    let status = if value == running {
        ResultStatus::Running
    } else if value == passed {
        ResultStatus::Passed
    } else {
        ResultStatus::Failed(value)
    };
    ResultObservation {
        address,
        value,
        status,
    }
}

/// Per-ROM execution settings for the test runner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RomTestDescriptor {
    pub mapping: Option<snes_core::MappingMode>,
    pub max_frames: Option<u64>,
    pub max_master_ticks: Option<u64>,
    pub result_protocol: Option<ResultProtocol>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RomRunResult {
    pub master_ticks: u64,
    pub stopped: bool,
    pub result: Option<ResultObservation>,
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
        self.load_image(RomImage::new(bytes))
    }

    /// Loads a ROM image from a file.
    pub fn load_path(&self, path: &Path) -> Result<Console, LoadError> {
        let bytes = fs::read(path).map_err(|_| LoadError::UnsupportedFormat)?;
        self.load_image(RomImage::load(bytes)?)
    }

    fn load_image(&self, image: RomImage) -> Result<Console, LoadError> {
        let image = match self.descriptor.mapping {
            Some(mapping) => image.with_mapping_override(mapping),
            None => image,
        };
        Console::new(image)
    }

    pub fn run(&self, console: &mut Console) -> RomRunResult {
        console.reset();
        let limit = self
            .descriptor
            .max_frames
            .map(|frames| frames.saturating_mul(MASTER_TICKS_PER_FRAME))
            .or(self.descriptor.max_master_ticks)
            .unwrap_or(0);
        let result = run_bounded(console, limit, self.descriptor.result_protocol.as_ref());
        RomRunResult {
            master_ticks: console.master_ticks(),
            stopped: console.is_stopped(),
            result: result.or_else(|| {
                self.descriptor
                    .result_protocol
                    .as_ref()
                    .map(|protocol| protocol.observe(console))
            }),
        }
    }
}

/// Runs a bounded console interval and, when supplied, latches the first
/// passing result observed after an instruction. Result-bearing ROMs may
/// briefly publish success before returning to an IPL or entering a new test
/// phase, so reading only the final machine state is insufficient. Non-running
/// values are left for the final observation because some protocols expose
/// handshake bytes before their documented failure code.
pub(crate) fn run_bounded(
    console: &mut Console,
    limit: u64,
    protocol: Option<&ResultProtocol>,
) -> Option<ResultObservation> {
    if protocol.is_none() {
        console.run_for_master_ticks(limit);
        return None;
    }

    let start = console.master_ticks();
    while console.master_ticks().saturating_sub(start) < limit {
        if console.is_stopped() {
            break;
        }
        let before = console.master_ticks();
        console.step_instruction();
        let observation = protocol.unwrap().observe(console);
        if matches!(observation.status, ResultStatus::Passed) {
            return Some(observation);
        }
        if console.master_ticks() == before {
            break;
        }
    }
    protocol.map(|protocol| protocol.observe(console))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_byte_protocol_matches_documented_status_values() {
        let protocol = ResultProtocol::memory_byte(Address24::new(0), 0, 1);
        assert_eq!(
            protocol.observe_memory(|_| 0),
            ResultObservation {
                address: Address24::new(0),
                value: 0,
                status: ResultStatus::Running,
            }
        );
        assert_eq!(protocol.observe_memory(|_| 1).status, ResultStatus::Passed);
        assert_eq!(
            protocol.observe_memory(|_| 0xff).status,
            ResultStatus::Failed(0xff)
        );
    }

    #[test]
    fn result_protocol_uses_full_24_bit_address() {
        let protocol = ResultProtocol::memory_byte(Address24::new(0x7e_1234), 0, 1);
        let observation = protocol.observe_memory(|address| {
            assert_eq!(address, Address24::new(0x7e_1234));
            1
        });
        assert_eq!(observation.status, ResultStatus::Passed);
    }
}
