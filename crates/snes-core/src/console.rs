//! Complete-machine ownership boundary.

use crate::{
    bus::{Address24, TraceSink},
    cartridge::{CartridgeDevice, LoadError, RomImage, RomOnlyCartridge},
    controller::ControllerState,
    cpu65816::{Cpu65816, CpuBus, CpuState, CpuStep},
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
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
        };
        self.cpu.reset_with_bus(&mut bus);
    }

    /// Advances one instruction according to the future CPU execution plan.
    pub fn step_instruction(&mut self) -> CpuStep {
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
        };
        let step = self.cpu.step_with_bus(&mut bus, None);
        self.timing.master_ticks = self.timing.master_ticks.saturating_add(step.master_ticks);
        step
    }

    pub fn step_instruction_traced(&mut self, trace: &mut dyn TraceSink) -> CpuStep {
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
        };
        let step = self.cpu.step_with_bus(&mut bus, Some(trace));
        self.timing.master_ticks = self.timing.master_ticks.saturating_add(step.master_ticks);
        step
    }

    /// Advances the machine by a number of master-clock ticks.
    pub fn run_for_master_ticks(&mut self, ticks: u64) {
        let start = self.timing.master_ticks;
        while self.timing.master_ticks.saturating_sub(start) < ticks {
            if self.cpu.state().stopped {
                break;
            }
            let before = self.timing.master_ticks;
            self.step_instruction();
            if self.timing.master_ticks == before {
                break;
            }
        }
    }

    /// Produces the next video frame.
    pub fn next_frame(&mut self) -> FrameOutput<'_> {
        FrameOutput::empty()
    }

    /// Reads a byte through the future complete SNES bus.
    pub fn read_memory(&self, address: Address24) -> u8 {
        let raw = address.get();
        if let Some(index) = wram_index(raw, self.wram.len()) {
            return self.wram[index];
        }
        0
    }

    pub fn is_stopped(&self) -> bool {
        self.cpu.state().stopped
    }

    pub fn master_ticks(&self) -> u64 {
        self.timing.master_ticks
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

struct ConsoleBus<'a> {
    wram: &'a mut [u8],
    cartridge: &'a mut dyn CartridgeDevice,
}

impl CpuBus for ConsoleBus<'_> {
    fn read(&mut self, address: Address24) -> u8 {
        let raw = address.get();
        if let Some(index) = wram_index(raw, self.wram.len()) {
            return self.wram[index];
        }
        self.cartridge.read(address)
    }

    fn write(&mut self, address: Address24, value: u8) {
        let raw = address.get();
        if let Some(index) = wram_index(raw, self.wram.len()) {
            self.wram[index] = value;
        } else {
            self.cartridge.write(address, value);
        }
    }
}

fn wram_index(address: u32, wram_len: usize) -> Option<usize> {
    let bank = address >> 16;
    let offset = address & 0xffff;
    if (0x7e..=0x7f).contains(&bank) {
        return Some(((address - 0x7e_0000) as usize) % wram_len);
    }
    if (bank <= 0x3f || (0x80..=0xbf).contains(&bank)) && offset < 0x2000 {
        return Some((offset as usize) % wram_len);
    }
    None
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
        let mut rom = vec![0xea; 0x8000];
        rom[0x7ffc] = 0x00;
        rom[0x7ffd] = 0x80;
        let mut console = Console::new(RomImage::new(rom)).unwrap();
        console.reset();
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

    #[test]
    fn console_executes_a_rom_through_the_machine_bus() {
        let mut rom = vec![0xea; 0x8000];
        rom[0] = 0xa9;
        rom[1] = 0x5a;
        rom[2] = 0x8f;
        rom[3] = 0x00;
        rom[4] = 0x00;
        rom[5] = 0x7e;
        rom[0x7ffc] = 0x00;
        rom[0x7ffd] = 0x80;
        let mut console = Console::new(RomImage::new(rom)).unwrap();
        console.reset();
        console.step_instruction();
        console.step_instruction();
        assert_eq!(console.read_memory(Address24::new(0x7e0000)), 0x5a);
    }

    #[test]
    fn console_exposes_low_wram_mirrors() {
        let mut console = Console::new(RomImage::new(vec![0; 8])).unwrap();
        let mut bus = ConsoleBus {
            wram: &mut console.wram,
            cartridge: &mut *console.cartridge,
        };
        bus.write(Address24::new(0x000123), 0xa5);
        assert_eq!(console.read_memory(Address24::new(0x7e0123)), 0xa5);
        assert_eq!(console.read_memory(Address24::new(0x800123)), 0xa5);
    }
}
