//! 5A22/65C816 CPU boundary.

use crate::{bus::TraceSink, instructions::InstructionId};

/// Result of one instruction execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuStep {
    pub instruction: InstructionId,
    pub cycles: u32,
    pub master_ticks: u64,
    pub access_count: u32,
}

impl CpuStep {
    pub(crate) fn placeholder() -> Self {
        Self {
            instruction: InstructionId::Placeholder,
            cycles: 0,
            master_ticks: 0,
            access_count: 0,
        }
    }
}

/// Serializable CPU state boundary used by snapshots.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuState {
    pub emulation_mode: bool,
    pub pc: u16,
    pub pbr: u8,
    pub status: u8,
}

/// Placeholder CPU state owner. Instruction semantics are deferred to the CPU
/// milestone and are intentionally absent from the initial scaffold.
#[derive(Clone, Debug, Default)]
pub struct Cpu65816;

impl Cpu65816 {
    pub(crate) fn reset(&mut self) {}

    pub(crate) fn step_instruction(&mut self, _trace: Option<&mut dyn TraceSink>) -> CpuStep {
        CpuStep::placeholder()
    }

    pub(crate) fn snapshot(&self) -> CpuState {
        CpuState {
            emulation_mode: true,
            ..CpuState::default()
        }
    }

    pub(crate) fn restore(&mut self, _state: CpuState) {}
}
