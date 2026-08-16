//! Complete-machine ownership boundary.

use crate::{
    apu::{Apu, AudioSample},
    bus::{Address24, TraceSink},
    cartridge::{CartridgeDevice, LoadError, RomImage, RomOnlyCartridge},
    controller::{ControllerPort, ControllerPorts},
    cpu65816::{Cpu65816, CpuBus, CpuState, CpuStep, InterruptKind},
    dma::Dma,
    ppu::{Ppu, SCREEN_HEIGHT, SCREEN_WIDTH},
    video::FrameOutput,
};

/// Approximate NTSC frame duration in the master-clock domain used by the
/// bounded headless runner. The renderer remains deterministic even before
/// scanline-level timing is introduced.
pub const MASTER_TICKS_PER_FRAME: u64 = 357_366;
pub const MASTER_TICKS_PER_SCANLINE: u64 = 1_364;
const NTSC_SCANLINES: u16 = 262;
const VBLANK_START_SCANLINE: u16 = 225;
const HBLANK_START_TICK: u64 = 1_096;
const CPU_IRQ_DISABLE: u8 = 0x04;

/// The complete SNES machine state owned by the core.
pub struct Console {
    cpu: Cpu65816,
    rom_identity: u64,
    #[allow(dead_code)]
    cartridge: Box<dyn CartridgeDevice>,
    wram: Vec<u8>,
    ppu: Ppu,
    dma: Dma,
    controller: ControllerPorts,
    timing: TimingState,
    framebuffer: Vec<u32>,
    apu: Apu,
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
            ppu: Ppu::default(),
            dma: Dma::default(),
            controller: ControllerPorts::default(),
            timing: TimingState::default(),
            framebuffer: vec![0; SCREEN_WIDTH * SCREEN_HEIGHT],
            apu: Apu::default(),
        })
    }

    /// Resets the machine state.
    pub fn reset(&mut self) {
        self.cpu.reset();
        self.ppu = Ppu::default();
        self.dma = Dma::default();
        self.controller = ControllerPorts::default();
        self.timing = TimingState::default();
        self.framebuffer.fill(0);
        self.apu = Apu::default();
        self.apu.boot_spc700();
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
            ppu: &mut self.ppu,
            controller: &mut self.controller,
            dma: Some(&mut self.dma),
            apu: &mut self.apu,
            timing: &mut self.timing,
        };
        self.cpu.reset_with_bus(&mut bus);
    }

    /// Advances one instruction according to the future CPU execution plan.
    pub fn step_instruction(&mut self) -> CpuStep {
        let pending = self.pending_interrupt();
        match pending {
            Some(InterruptKind::Nmi) => self.timing.pending_nmi = false,
            Some(InterruptKind::Irq) => self.timing.pending_irq = false,
            Some(InterruptKind::Cop) | None => {}
        }
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
            ppu: &mut self.ppu,
            controller: &mut self.controller,
            dma: Some(&mut self.dma),
            apu: &mut self.apu,
            timing: &mut self.timing,
        };
        let step = match pending {
            Some(InterruptKind::Nmi) => {
                self.cpu
                    .service_interrupt(&mut bus, InterruptKind::Nmi, None)
            }
            Some(InterruptKind::Irq) => {
                self.cpu
                    .service_interrupt(&mut bus, InterruptKind::Irq, None)
            }
            Some(InterruptKind::Cop) | None => self.cpu.step_with_bus(&mut bus, None),
        };
        drop(bus);
        self.finish_cpu_step(step)
    }

    pub fn step_instruction_traced(&mut self, trace: &mut dyn TraceSink) -> CpuStep {
        let pending = self.pending_interrupt();
        match pending {
            Some(InterruptKind::Nmi) => self.timing.pending_nmi = false,
            Some(InterruptKind::Irq) => self.timing.pending_irq = false,
            Some(InterruptKind::Cop) | None => {}
        }
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
            ppu: &mut self.ppu,
            controller: &mut self.controller,
            dma: Some(&mut self.dma),
            apu: &mut self.apu,
            timing: &mut self.timing,
        };
        let step = match pending {
            Some(InterruptKind::Nmi) => {
                self.cpu
                    .service_interrupt(&mut bus, InterruptKind::Nmi, Some(trace))
            }
            Some(InterruptKind::Irq) => {
                self.cpu
                    .service_interrupt(&mut bus, InterruptKind::Irq, Some(trace))
            }
            Some(InterruptKind::Cop) | None => self.cpu.step_with_bus(&mut bus, Some(trace)),
        };
        drop(bus);
        self.finish_cpu_step(step)
    }

    pub fn request_interrupt(&mut self, kind: InterruptKind) -> CpuStep {
        match kind {
            InterruptKind::Nmi => self.timing.pending_nmi = false,
            InterruptKind::Irq => self.timing.pending_irq = false,
            InterruptKind::Cop => {}
        }
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
            ppu: &mut self.ppu,
            controller: &mut self.controller,
            dma: Some(&mut self.dma),
            apu: &mut self.apu,
            timing: &mut self.timing,
        };
        let step = self.cpu.service_interrupt(&mut bus, kind, None);
        drop(bus);
        self.finish_cpu_step(step)
    }

    fn pending_interrupt(&self) -> Option<InterruptKind> {
        if self.cpu.state().stopped {
            return None;
        }
        if self.timing.pending_nmi {
            return Some(InterruptKind::Nmi);
        }
        if self.timing.pending_irq && self.cpu.state().status & CPU_IRQ_DISABLE == 0 {
            return Some(InterruptKind::Irq);
        }
        None
    }

    fn finish_cpu_step(&mut self, step: CpuStep) -> CpuStep {
        self.timing.master_ticks = self.timing.master_ticks.saturating_add(step.master_ticks);
        self.apu.step_spc700();
        self.run_pending_dma();
        self.run_hdma_boundaries();
        step
    }

    fn run_pending_dma(&mut self) {
        let mut dma = self.dma;
        let pending = dma.take_pending();
        if pending == 0 {
            self.dma = dma;
            return;
        }
        let mut bus = ConsoleBus {
            wram: &mut self.wram,
            cartridge: &mut *self.cartridge,
            ppu: &mut self.ppu,
            controller: &mut self.controller,
            dma: None,
            apu: &mut self.apu,
            timing: &mut self.timing,
        };
        let transfers = dma.execute(pending, &mut bus);
        drop(bus);
        self.dma = dma;
        self.timing.master_ticks = self
            .timing
            .master_ticks
            .saturating_add(u64::from(transfers) * 6);
    }

    fn run_hdma_boundaries(&mut self) {
        while self.timing.master_ticks >= self.timing.next_hdma_tick {
            // A malformed or saturated snapshot must not turn the bounded
            // scheduler into an infinite loop once the absolute tick domain
            // can no longer represent another scanline boundary.
            if self.timing.next_hdma_tick == u64::MAX {
                break;
            }
            if self.timing.scanline == 0 {
                self.dma.begin_hdma_frame();
            }
            let mut dma = self.dma;
            let mut bus = ConsoleBus {
                wram: &mut self.wram,
                cartridge: &mut *self.cartridge,
                ppu: &mut self.ppu,
                controller: &mut self.controller,
                dma: None,
                apu: &mut self.apu,
                timing: &mut self.timing,
            };
            let transfers = dma.run_hdma_scanline(&mut bus);
            drop(bus);
            self.dma = dma;
            self.update_h_timer(true);
            let next_scanline = (self.timing.scanline + 1) % NTSC_SCANLINES;
            self.update_vblank_and_v_timer(next_scanline);
            self.timing.scanline = next_scanline;
            self.timing.h_timer_fired = false;
            self.timing.master_ticks = self
                .timing
                .master_ticks
                .saturating_add(u64::from(transfers) * 6);
            self.timing.next_hdma_tick = self
                .timing
                .next_hdma_tick
                .checked_add(MASTER_TICKS_PER_SCANLINE)
                .unwrap_or(u64::MAX);
        }
        self.update_h_timer(false);
    }

    fn update_h_timer(&mut self, force_line_end: bool) {
        if force_line_end {
            if !self.timing.h_timer_fired {
                self.timing.h_timer_fired = true;
                self.raise_h_timer_irq();
            }
            return;
        }

        let line_start = self
            .timing
            .next_hdma_tick
            .saturating_sub(MASTER_TICKS_PER_SCANLINE);
        let phase = self.timing.master_ticks.saturating_sub(line_start);
        if !self.timing.h_timer_fired && phase >= u64::from(self.timing.h_timer) {
            self.timing.h_timer_fired = true;
            self.raise_h_timer_irq();
        }
    }

    fn raise_h_timer_irq(&mut self) {
        let mode = (self.timing.nmitimen >> 4) & 0x03;
        if mode & 0x01 != 0 && (mode & 0x02 == 0 || self.timing.scanline == self.timing.v_timer) {
            self.timing.timeup = true;
            self.timing.pending_irq = true;
        }
    }

    fn update_vblank_and_v_timer(&mut self, scanline: u16) {
        if scanline == VBLANK_START_SCANLINE {
            self.timing.rdnmi = true;
            if self.timing.nmitimen & 0x80 != 0 {
                self.timing.pending_nmi = true;
            }
        } else if scanline == 0 {
            self.timing.rdnmi = false;
        }

        let mode = (self.timing.nmitimen >> 4) & 0x03;
        if scanline == self.timing.v_timer && mode == 0x02 {
            self.timing.timeup = true;
            self.timing.pending_irq = true;
        }
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

    pub fn run_for_frames(&mut self, frames: u64) {
        self.run_for_master_ticks(frames.saturating_mul(MASTER_TICKS_PER_FRAME));
    }

    /// Produces the next video frame.
    pub fn next_frame(&mut self) -> FrameOutput<'_> {
        self.ppu.render_frame(&mut self.framebuffer);
        FrameOutput {
            pixels: &self.framebuffer,
            width: SCREEN_WIDTH as u16,
            height: SCREEN_HEIGHT as u16,
        }
    }

    /// Renders a bounded batch of audio samples from the owned APU mixer.
    pub fn render_audio(&mut self, sample_count: usize) -> Vec<AudioSample> {
        self.apu.render_audio(sample_count)
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
            cartridge: self.cartridge.save_state(),
            apu: self.apu.clone(),
            ppu: self.ppu.clone(),
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
        if !self.cartridge.restore_state(&state.cartridge) {
            return Err(RestoreError::InvalidCartridgeState);
        }
        self.cpu.restore(state.cpu);
        self.wram.copy_from_slice(&state.wram);
        self.ppu = state.ppu.clone();
        self.dma = state.dma;
        self.apu = state.apu.clone();
        self.controller = state.controller;
        self.timing = state.timing;
        Ok(())
    }
}

struct ConsoleBus<'a> {
    wram: &'a mut [u8],
    cartridge: &'a mut dyn CartridgeDevice,
    ppu: &'a mut Ppu,
    controller: &'a mut ControllerPorts,
    dma: Option<&'a mut Dma>,
    apu: &'a mut Apu,
    timing: &'a mut TimingState,
}

impl CpuBus for ConsoleBus<'_> {
    fn read(&mut self, address: Address24) -> u8 {
        let raw = address.get();
        if let Some(index) = wram_index(raw, self.wram.len()) {
            return self.wram[index];
        }
        if let Some(register) = ppu_register(raw) {
            return self.ppu.read_register(register);
        }
        if let Some(port) = controller_port(raw) {
            return self.controller.read_port(port, 0);
        }
        if let Some(port) = apu_port(raw) {
            return self.apu.read_cpu_port(port);
        }
        if let Some(register) = timing_register(raw) {
            return self.timing.read_register(register);
        }
        if let Some(register) = dma_register(raw) {
            return self
                .dma
                .as_deref_mut()
                .map_or(0, |dma| dma.read_register(register));
        }
        self.cartridge.read(address)
    }

    fn write(&mut self, address: Address24, value: u8) {
        let raw = address.get();
        if let Some(index) = wram_index(raw, self.wram.len()) {
            self.wram[index] = value;
        } else if let Some(register) = ppu_register(raw) {
            self.ppu.write_register(register, value);
        } else if let Some(port) = controller_port(raw) {
            if port == ControllerPort::Port1 {
                self.controller.write_strobe(value);
            }
        } else if let Some(port) = apu_port(raw) {
            self.apu.write_cpu_port(port, value);
        } else if let Some(register) = timing_register(raw) {
            self.timing.write_register(register, value);
        } else if let Some(register) = dma_register(raw) {
            if let Some(dma) = self.dma.as_deref_mut() {
                dma.write_register(register, value);
            }
        } else {
            self.cartridge.write(address, value);
        }
    }
}

fn timing_register(address: u32) -> Option<u16> {
    let offset = address & 0xffff;
    (is_system_bank(address) && matches!(offset, 0x4200 | 0x4207..=0x420a | 0x4210..=0x4212))
        .then_some(offset as u16)
}

fn is_system_bank(address: u32) -> bool {
    let bank = address >> 16;
    bank <= 0x3f || (0x80..=0xbf).contains(&bank)
}

fn controller_port(address: u32) -> Option<ControllerPort> {
    if !is_system_bank(address) {
        return None;
    }
    match address & 0xffff {
        0x4016 => Some(ControllerPort::Port1),
        0x4017 => Some(ControllerPort::Port2),
        _ => None,
    }
}

fn dma_register(address: u32) -> Option<u16> {
    let offset = address & 0xffff;
    (is_system_bank(address) && (0x4200..=0x437f).contains(&offset)).then_some(offset as u16)
}

fn apu_port(address: u32) -> Option<usize> {
    let offset = address & 0xffff;
    if is_system_bank(address) && (0x2140..=0x2143).contains(&offset) {
        Some((offset - 0x2140) as usize)
    } else {
        None
    }
}

fn ppu_register(address: u32) -> Option<u16> {
    let offset = address & 0xffff;
    (is_system_bank(address) && (0x2100..=0x213f).contains(&offset)).then_some(offset as u16)
}

fn wram_index(address: u32, wram_len: usize) -> Option<usize> {
    let bank = address >> 16;
    let offset = address & 0xffff;
    if (0x7e..=0x7f).contains(&bank) {
        return Some(((address - 0x7e_0000) as usize) % wram_len);
    }
    if is_system_bank(address) && offset < 0x2000 {
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
    pub cartridge: Vec<u8>,
    pub apu: Apu,
    pub ppu: Ppu,
    pub dma: Dma,
    pub controller: ControllerPorts,
    pub timing: TimingState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimingState {
    pub master_ticks: u64,
    pub scanline: u16,
    pub next_hdma_tick: u64,
    pub nmitimen: u8,
    pub rdnmi: bool,
    pub timeup: bool,
    pub h_timer: u16,
    pub v_timer: u16,
    pub pending_nmi: bool,
    pub pending_irq: bool,
    pub h_timer_fired: bool,
}

impl Default for TimingState {
    fn default() -> Self {
        Self {
            master_ticks: 0,
            scanline: 0,
            next_hdma_tick: MASTER_TICKS_PER_SCANLINE,
            nmitimen: 0,
            rdnmi: false,
            timeup: false,
            h_timer: 0,
            v_timer: 0,
            pending_nmi: false,
            pending_irq: false,
            h_timer_fired: false,
        }
    }
}

impl TimingState {
    fn read_register(&mut self, address: u16) -> u8 {
        match address {
            0x4200 => self.nmitimen,
            0x4207 => self.h_timer as u8,
            0x4208 => (self.h_timer >> 8) as u8,
            0x4209 => self.v_timer as u8,
            0x420a => (self.v_timer >> 8) as u8,
            0x4210 => {
                let value = (if self.rdnmi { 0x80 } else { 0 }) | 2;
                self.rdnmi = false;
                value
            }
            0x4211 => {
                let value = if self.timeup { 0x80 } else { 0 };
                self.timeup = false;
                self.pending_irq = false;
                value
            }
            0x4212 => {
                let line_start = self
                    .next_hdma_tick
                    .saturating_sub(MASTER_TICKS_PER_SCANLINE);
                let phase = self.master_ticks.saturating_sub(line_start);
                let hblank = phase >= HBLANK_START_TICK;
                (if self.scanline >= VBLANK_START_SCANLINE {
                    0x80
                } else {
                    0
                }) | if hblank { 0x40 } else { 0 }
            }
            _ => 0,
        }
    }

    fn write_register(&mut self, address: u16, value: u8) {
        match address {
            0x4200 => {
                let old_nmi_enabled = self.nmitimen & 0x80 != 0;
                self.nmitimen = value;
                if !old_nmi_enabled && value & 0x80 != 0 && self.rdnmi {
                    self.pending_nmi = true;
                }
            }
            0x4207 => self.h_timer = (self.h_timer & 0x100) | u16::from(value),
            0x4208 => self.h_timer = (self.h_timer & 0xff) | (u16::from(value & 1) << 8),
            0x4209 => self.v_timer = (self.v_timer & 0x100) | u16::from(value),
            0x420a => self.v_timer = (self.v_timer & 0xff) | (u16::from(value & 1) << 8),
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreError {
    UnsupportedVersion(u16),
    RomMismatch,
    InvalidMemorySize,
    InvalidCartridgeState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bus::{AccessSpeed, VecTrace},
        cartridge::{CartridgeDevice, RomImage},
    };

    struct ProbeCartridge {
        read_value: u8,
        writes: Vec<(u32, u8)>,
    }

    impl CartridgeDevice for ProbeCartridge {
        fn read(&mut self, _address: Address24) -> u8 {
            self.read_value
        }

        fn write(&mut self, address: Address24, value: u8) {
            self.writes.push((address.get(), value));
        }

        fn access_speed(&self, _address: Address24) -> AccessSpeed {
            AccessSpeed::Fast
        }
    }

    #[test]
    fn unmapped_low_bus_addresses_do_not_underflow_apu_port_decode() {
        assert_eq!(apu_port(0x000000), None);
        assert_eq!(apu_port(0x002140), Some(0));
    }

    #[test]
    fn system_devices_only_decode_in_system_banks_and_keep_priority() {
        let mut wram = vec![0; 128 * 1024];
        let mut ppu = Ppu::default();
        let mut controller = ControllerPorts::default();
        controller.port1.set_buttons(1);
        controller.port2.set_buttons(1 << 1);
        controller.write_strobe(1);
        let mut dma = Dma::default();
        let mut apu = Apu::default();
        let mut cartridge = ProbeCartridge {
            read_value: 0xee,
            writes: Vec::new(),
        };
        let mut bus = ConsoleBus {
            wram: &mut wram,
            cartridge: &mut cartridge,
            ppu: &mut ppu,
            controller: &mut controller,
            dma: Some(&mut dma),
            apu: &mut apu,
            timing: &mut TimingState::default(),
        };

        // WRAM, PPU, APU, controller, and DMA all win in the system banks.
        bus.write(Address24::new(0x000123), 0xa5);
        assert_eq!(bus.read(Address24::new(0x800123)), 0xa5);
        bus.write(Address24::new(0x002100), 0x12);
        assert_eq!(bus.read(Address24::new(0x802100)), 0x12);
        bus.write(Address24::new(0x002140), 0x34);
        // CPU writes feed the SPC700 input latch; the CPU reads the separate
        // SPC700 output latch until the SPC writes a response.
        assert_eq!(bus.read(Address24::new(0x802140)), 0);
        assert_eq!(bus.read(Address24::new(0x004016)), 1);
        assert_eq!(bus.read(Address24::new(0x004017)), 0x1c);
        bus.write(Address24::new(0x004017), 0);
        bus.write(Address24::new(0x804016), 0);
        assert_eq!(bus.read(Address24::new(0x804016)), 1);
        assert_eq!(bus.read(Address24::new(0x804017)), 0x1c);
        bus.write(Address24::new(0x00430b), 0x01);
        assert_eq!(bus.read(Address24::new(0x80430b)), 0);
        bus.write(Address24::new(0x004302), 0x78);
        assert_eq!(bus.read(Address24::new(0x804302)), 0x78);

        // The same offsets in a cartridge bank must reach the cartridge and
        // must not mutate any of the system-device state above.
        assert_eq!(bus.read(Address24::new(0x400123)), 0xee);
        assert_eq!(bus.read(Address24::new(0x402100)), 0xee);
        assert_eq!(bus.read(Address24::new(0x402140)), 0xee);
        assert_eq!(bus.read(Address24::new(0x404016)), 0xee);
        assert_eq!(bus.read(Address24::new(0x404017)), 0xee);
        assert_eq!(bus.read(Address24::new(0x404302)), 0xee);
        bus.write(Address24::new(0x40430b), 0x02);
        bus.write(Address24::new(0x404302), 0x99);
        assert_eq!(bus.read(Address24::new(0x004302)), 0x78);
        drop(bus);
        assert_eq!(cartridge.writes, vec![(0x40430b, 0x02), (0x404302, 0x99)]);
    }

    #[test]
    fn dma_register_accesses_do_not_fall_through_during_dma_bus_use() {
        let mut wram = vec![0; 128 * 1024];
        let mut ppu = Ppu::default();
        let mut controller = ControllerPorts::default();
        let mut apu = Apu::default();
        let mut cartridge = ProbeCartridge {
            read_value: 0xee,
            writes: Vec::new(),
        };
        let mut bus = ConsoleBus {
            wram: &mut wram,
            cartridge: &mut cartridge,
            ppu: &mut ppu,
            controller: &mut controller,
            dma: None,
            apu: &mut apu,
            timing: &mut TimingState::default(),
        };

        assert_eq!(bus.read(Address24::new(0x00420c)), 0);
        bus.write(Address24::new(0x00420b), 1);
        drop(bus);
        assert!(cartridge.writes.is_empty());
    }

    #[test]
    fn hdma_runs_at_console_scanline_boundaries() {
        let mut rom = vec![0xea; 0x8000];
        rom[0x7ffc] = 0x00;
        rom[0x7ffd] = 0x80;
        let mut console = Console::new(RomImage::new(rom)).unwrap();
        console.reset();
        console.dma.channels[0] = crate::dma::DmaChannel {
            destination: 0x18,
            source_address: 0x1000,
            ..crate::dma::DmaChannel::default()
        };
        console.dma.hdma_enable = 1;
        console.wram[0x1000..0x1003].copy_from_slice(&[1, 0xaa, 0]);
        console.timing.next_hdma_tick = 1;

        console.step_instruction();

        assert_eq!(console.ppu.vram[0] as u8, 0xaa);
        assert_eq!(console.timing.scanline, 1);
    }

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
    fn snapshot_restores_scanline_scheduler_state() {
        let mut console = Console::new(RomImage::new(vec![0xea; 8])).unwrap();
        console.timing = TimingState {
            master_ticks: MASTER_TICKS_PER_SCANLINE * 17 + 9,
            scanline: 17,
            next_hdma_tick: MASTER_TICKS_PER_SCANLINE * 18,
            ..TimingState::default()
        };
        console.dma.hdma_channels[2] = crate::dma::HdmaChannelState {
            table_address: 0x3456,
            line_counter: 3,
            initialized: true,
            transfer_pending: true,
            ..crate::dma::HdmaChannelState::default()
        };
        let snapshot = console.snapshot();

        console.timing = TimingState::default();
        console.dma.hdma_channels[2] = crate::dma::HdmaChannelState::default();
        console.restore(&snapshot).unwrap();

        assert_eq!(console.timing, snapshot.timing);
        assert_eq!(console.dma.hdma_channels[2], snapshot.dma.hdma_channels[2]);
    }

    #[test]
    fn traced_and_untraced_steps_have_same_summary() {
        let mut bytes = vec![0xea; 0x8000];
        bytes[0x7ffc] = 0x00;
        bytes[0x7ffd] = 0x80;
        let rom = RomImage::new(bytes);
        let mut plain = Console::new(rom.clone()).unwrap();
        let mut traced = Console::new(rom).unwrap();
        plain.reset();
        traced.reset();
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
    fn console_reset_enters_the_spc700_ipl_uploader() {
        let mut rom = vec![0; 0x8000];
        rom[0x7ffc] = 0x00;
        rom[0x7ffd] = 0x80;
        rom[0x0000] = 0xdb;
        let image = RomImage::new(rom);
        let mut console = Console::new(image).unwrap();

        console.reset();

        assert_eq!(console.apu.spc700.pc, 0xffc0);
        assert_eq!(console.apu.control, 0xb0);
    }

    #[test]
    fn console_exposes_low_wram_mirrors() {
        let mut console = Console::new(RomImage::new(vec![0; 8])).unwrap();
        let mut bus = ConsoleBus {
            wram: &mut console.wram,
            cartridge: &mut *console.cartridge,
            ppu: &mut console.ppu,
            controller: &mut console.controller,
            dma: Some(&mut console.dma),
            apu: &mut console.apu,
            timing: &mut console.timing,
        };
        bus.write(Address24::new(0x000123), 0xa5);
        assert_eq!(console.read_memory(Address24::new(0x7e0123)), 0xa5);
        assert_eq!(console.read_memory(Address24::new(0x800123)), 0xa5);
    }

    #[test]
    fn timing_registers_route_before_dma_and_status_reads_clear_latches() {
        let mut console = Console::new(RomImage::new(vec![0xea; 8])).unwrap();
        console.timing.rdnmi = true;
        console.timing.timeup = true;
        let mut bus = ConsoleBus {
            wram: &mut console.wram,
            cartridge: &mut *console.cartridge,
            ppu: &mut console.ppu,
            controller: &mut console.controller,
            dma: Some(&mut console.dma),
            apu: &mut console.apu,
            timing: &mut console.timing,
        };

        bus.write(Address24::new(0x004200), 0xb1);
        bus.write(Address24::new(0x004207), 0x34);
        bus.write(Address24::new(0x804208), 0x01);
        bus.write(Address24::new(0x004209), 0x56);
        bus.write(Address24::new(0x00420a), 0x01);
        assert_eq!(bus.read(Address24::new(0x004200)), 0xb1);
        assert_eq!(bus.read(Address24::new(0x804207)), 0x34);
        assert_eq!(bus.read(Address24::new(0x004208)), 0x01);
        assert_eq!(bus.read(Address24::new(0x004209)), 0x56);
        assert_eq!(bus.read(Address24::new(0x00420a)), 0x01);

        assert_eq!(bus.read(Address24::new(0x004210)) & 0x80, 0x80);
        assert_eq!(bus.read(Address24::new(0x804210)) & 0x80, 0);
        assert_eq!(bus.read(Address24::new(0x004211)) & 0x80, 0x80);
        assert_eq!(bus.read(Address24::new(0x004211)) & 0x80, 0);
    }

    #[test]
    fn hvbjoy_reports_hblank_and_vblank_from_scanline_scheduler() {
        let mut console = Console::new(RomImage::new(vec![0xea; 8])).unwrap();
        console.timing.master_ticks = HBLANK_START_TICK - 1;
        let mut bus = ConsoleBus {
            wram: &mut console.wram,
            cartridge: &mut *console.cartridge,
            ppu: &mut console.ppu,
            controller: &mut console.controller,
            dma: Some(&mut console.dma),
            apu: &mut console.apu,
            timing: &mut console.timing,
        };

        assert_eq!(bus.read(Address24::new(0x004212)) & 0x40, 0);
        drop(bus);
        console.timing.master_ticks = HBLANK_START_TICK;
        let mut bus = ConsoleBus {
            wram: &mut console.wram,
            cartridge: &mut *console.cartridge,
            ppu: &mut console.ppu,
            controller: &mut console.controller,
            dma: Some(&mut console.dma),
            apu: &mut console.apu,
            timing: &mut console.timing,
        };
        assert_eq!(bus.read(Address24::new(0x804212)) & 0x40, 0x40);
        drop(bus);

        console.timing.scanline = VBLANK_START_SCANLINE;
        let mut bus = ConsoleBus {
            wram: &mut console.wram,
            cartridge: &mut *console.cartridge,
            ppu: &mut console.ppu,
            controller: &mut console.controller,
            dma: Some(&mut console.dma),
            apu: &mut console.apu,
            timing: &mut console.timing,
        };
        assert_eq!(bus.read(Address24::new(0x004212)) & 0x80, 0x80);
    }

    #[test]
    fn vblank_sets_rdnmi_and_services_enabled_nmi_on_next_instruction() {
        let mut rom = vec![0xea; 0x8000];
        rom[0x7ffa] = 0x34;
        rom[0x7ffb] = 0x12;
        rom[0x7ffc] = 0x00;
        rom[0x7ffd] = 0x80;
        let mut console = Console::new(RomImage::new(rom)).unwrap();
        console.reset();
        console.timing.nmitimen = 0x80;
        console.timing.scanline = VBLANK_START_SCANLINE - 1;
        console.timing.master_ticks = MASTER_TICKS_PER_SCANLINE * u64::from(VBLANK_START_SCANLINE);
        console.timing.next_hdma_tick = console.timing.master_ticks;

        console.run_hdma_boundaries();

        assert!(console.timing.rdnmi);
        assert!(console.timing.pending_nmi);
        console.step_instruction();
        assert_eq!(console.cpu.state().pc, 0x1234);
        assert!(!console.timing.pending_nmi);
    }

    #[test]
    fn h_timer_raises_irq_only_after_h_counter_target_and_respects_i_flag() {
        let mut rom = vec![0xea; 0x8000];
        rom[0x7ffe] = 0x78;
        rom[0x7fff] = 0x56;
        rom[0x7ffc] = 0x00;
        rom[0x7ffd] = 0x80;
        let mut console = Console::new(RomImage::new(rom)).unwrap();
        console.reset();
        console.timing.nmitimen = 0x10;
        console.timing.h_timer = 100;
        console.timing.master_ticks = 99;
        console.timing.h_timer_fired = false;
        console.run_hdma_boundaries();
        assert!(!console.timing.timeup);

        console.timing.master_ticks = 100;
        console.run_hdma_boundaries();
        assert!(console.timing.timeup);
        assert!(console.timing.pending_irq);

        // The reset I flag masks the request; clearing it lets the existing
        // Cpu65816 interrupt-entry API service the latched IRQ.
        let mut cpu_state = console.cpu.state();
        cpu_state.status &= !CPU_IRQ_DISABLE;
        console.cpu.restore(cpu_state);
        console.step_instruction();
        assert_eq!(console.cpu.state().pc, 0x5678);
        assert!(!console.timing.pending_irq);
    }

    #[test]
    fn interrupt_latches_are_part_of_snapshot_determinism() {
        let mut console = Console::new(RomImage::new(vec![0xea; 8])).unwrap();
        console.timing = TimingState {
            nmitimen: 0xb0,
            rdnmi: true,
            timeup: true,
            h_timer: 123,
            v_timer: 45,
            pending_nmi: true,
            pending_irq: true,
            h_timer_fired: true,
            ..TimingState::default()
        };
        let snapshot = console.snapshot();
        console.timing = TimingState::default();
        console.restore(&snapshot).unwrap();
        assert_eq!(console.timing, snapshot.timing);
    }

    #[test]
    fn console_audio_rendering_reaches_owned_mixer_deterministically() {
        fn configure_audio(console: &mut Console) {
            console.apu.dsp[0x5d] = 0x10;
            console.apu.ram[0x1000..0x1004].copy_from_slice(&[0x00, 0x20, 0x00, 0x20]);
            console.apu.ram[0x2000] = 0x03;
            console.apu.ram[0x2001] = 0x77;
            console.apu.dsp[0x00] = 0x7f;
            console.apu.dsp[0x01] = 0x7f;
            console.apu.dsp[0x03] = 0x10;
            console.apu.dsp[0x04] = 0;
            console.apu.dsp[0x07] = 0x7f;
            console.apu.dsp[0x0c] = 0x7f;
            console.apu.dsp[0x1c] = 0x7f;
            console.apu.write_dsp_address(0x4c);
            console.apu.write_dsp_data(0x01);
        }

        let rom = RomImage::new(vec![0xea; 0x8000]);
        let mut first = Console::new(rom.clone()).unwrap();
        let mut second = Console::new(rom).unwrap();
        configure_audio(&mut first);
        configure_audio(&mut second);

        let first_samples = first.render_audio(2);
        let second_samples = second.render_audio(2);

        assert_eq!(first_samples, second_samples);
        assert!(first_samples[0].left > 20_000);
        assert_eq!(first_samples[0].left, first_samples[0].right);
        assert_eq!(first.apu.dsp[0x09], 0x70);
    }
}
