//! Frontend-neutral building blocks for the SNES emulator.
//!
//! The initial crate exposes the subsystem boundaries from the project plan.
//! Hardware behavior will be implemented behind these boundaries in later
//! milestones.

pub mod bus;
pub mod apu;
pub mod cartridge;
pub mod console;
pub mod controller;
pub mod cpu65816;
pub mod dma;
pub mod instructions;
pub mod ppu;
pub mod rewind;
pub mod video;

pub use bus::{AccessSpeed, Address24, BusAccess, BusAccessKind, TraceSink, VecTrace};
pub use apu::Apu;
pub use cartridge::{CartridgeDevice, LoadError, MappingMode, RomImage, RomMetadata};
pub use console::{
    Console, ConsoleState, RestoreError, TimingState, MASTER_TICKS_PER_FRAME, SNAPSHOT_VERSION,
};
pub use cpu65816::{Cpu65816, CpuBus, CpuState, CpuStep};
pub use rewind::RewindBuffer;
pub use video::FrameOutput;
