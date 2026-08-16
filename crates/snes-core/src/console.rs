//! Complete-machine ownership boundary.

use crate::{
    bus::{Address24, TraceSink},
    cartridge::{CartridgeDevice, LoadError, RomImage, RomOnlyCartridge},
    controller::ControllerState,
    cpu65816::{Cpu65816, CpuState, CpuStep},
    dma::Dma,
    ppu::Ppu,
    video::FrameOutput,
};

/// The complete SNES machine state owned by the core.
pub struct Console {
    cpu: Cpu65816,
    rom_identity: u64,
    #[allow(dead_code)]
    cartridge: Box<dyn CartridgeDevice>,
    wram: Vec<u8>,
    ppu: Ppu,
    dma: Dma,
    controller: ControllerState,
    timing: TimingState,
}

impl Console {
    /// Creates a console around a supplied ROM image.
    pub fn new(rom: RomImage) -> Result<Self, LoadError> {
        if rom.is_empty() {
            return Err(LoadError::EmptyImage);
        }

        let rom_identity = rom.identity();
        Ok(Self {
            cpu: Cpu65816::default(),
            rom_identity,
            cartridge: Box::new(RomOnlyCartridge::new(rom)),
            wram: vec![0; 128 * 1024],
            ppu: Ppu,
            dma: Dma,
            controller: ControllerState::default(),
            timing: TimingState::default(),
        })
    }

    /// Resets the machine state.
    pub fn reset(&mut self) {
        self.cpu.reset();
    }

    /// Advances one instruction according to the future CPU execution plan.
    pub fn step_instruction(&mut self) -> CpuStep {
        self.cpu.step_instruction(None)
    }

    pub fn step_instruction_traced(&mut self, trace: &mut dyn TraceSink) -> CpuStep {
        self.cpu.step_instruction(Some(trace))
    }

    /// Advances the machine by a number of master-clock ticks.
    pub fn run_for_master_ticks(&mut self, ticks: u64) {
        self.timing.master_ticks = self.timing.master_ticks.saturating_add(ticks);
    }

    /// Produces the next video frame.
    pub fn next_frame(&mut self) -> FrameOutput<'_> {
        FrameOutput::empty()
    }

    /// Reads a byte through the future complete SNES bus.
    pub fn read_memory(&self, address: Address24) -> u8 {
        let raw = address.get();
        if (0x7e_0000..=0x7f_ffff).contains(&raw) {
            return self.wram[(raw as usize - 0x7e_0000) % self.wram.len()];
        }
        0
    }

    /// Captures a deterministic machine-state snapshot boundary.
    pub fn snapshot(&self) -> ConsoleState {
        ConsoleState {
            version: SNAPSHOT_VERSION,
            rom_identity: self.rom_identity,
            cpu: self.cpu.snapshot(),
            wram: self.wram.clone(),
            ppu: self.ppu,
            dma: self.dma,
            controller: self.controller,
            timing: self.timing,
        }
    }

    pub fn restore(&mut self, state: &ConsoleState) -> Result<(), RestoreError> {
        if state.version != SNAPSHOT_VERSION {
            return Err(RestoreError::UnsupportedVersion(state.version));
        }
        if state.rom_identity != self.rom_identity {
            return Err(RestoreError::RomMismatch);
        }
        if state.wram.len() != self.wram.len() {
            return Err(RestoreError::InvalidMemorySize);
        }
        self.cpu.restore(state.cpu);
        self.wram.copy_from_slice(&state.wram);
        self.ppu = state.ppu;
        self.dma = state.dma;
        self.controller = state.controller;
        self.timing = state.timing;
        Ok(())
    }
}

/// Serializable snapshot boundary for deterministic replay tooling.
pub const SNAPSHOT_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsoleState {
    pub version: u16,
    pub rom_identity: u64,
    pub cpu: CpuState,
    pub wram: Vec<u8>,
    pub ppu: Ppu,
    pub dma: Dma,
    pub controller: ControllerState,
    pub timing: TimingState,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TimingState {
    pub master_ticks: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreError {
    UnsupportedVersion(u16),
    RomMismatch,
    InvalidMemorySize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bus::VecTrace, cartridge::RomImage};

    #[test]
    fn snapshot_restores_memory_and_timing() {
        let mut console = Console::new(RomImage::new(vec![1, 2, 3])).unwrap();
        console.run_for_master_ticks(123);
        let snapshot = console.snapshot();
        console.run_for_master_ticks(456);
        assert_ne!(console.snapshot(), snapshot);
        console.restore(&snapshot).unwrap();
        assert_eq!(console.snapshot(), snapshot);
    }

    #[test]
    fn traced_and_untraced_steps_have_same_summary() {
        let rom = RomImage::new(vec![0; 8]);
        let mut plain = Console::new(rom.clone()).unwrap();
        let mut traced = Console::new(rom).unwrap();
        let plain_step = plain.step_instruction();
        let mut trace = VecTrace::default();
        let traced_step = traced.step_instruction_traced(&mut trace);
        assert_eq!(plain_step, traced_step);
        assert_eq!(trace.as_slice().len(), 1);
    }
}
