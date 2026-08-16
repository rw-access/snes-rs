# Accuracy ledger

This document records the sources of truth that future implementations should
use. The implementation is intentionally incremental; the ledger distinguishes
tested behavior from remaining accuracy work rather than treating a ROM that
merely runs as a compatibility pass.

| Subsystem | Primary references | Planned evidence |
| --- | --- | --- |
| 5A22 / 65C816 CPU | [CPU vectors](https://snes.nesdev.org/wiki/CPU_vectors), [65C816](https://snes.nesdev.org/wiki/65C816), [65816 opcodes](https://undisbeliever.net/snesdev/65816-opcodes.html) | Rust unit tests, SingleStepTests, bus traces |
| Timing and bus | [SNES timing](https://snes.nesdev.org/wiki/Timing) | master-clock bounds and ordered access traces |
| Cartridge formats | [ROM header](https://snes.nesdev.org/wiki/ROM_header), [ROM file formats](https://snes.nesdev.org/wiki/ROM_file_formats), [memory map](https://snes.nesdev.org/wiki/Memory_map) | LoROM/HiROM loader tests and synthetic ROMs |
| PPU | [PPU registers](https://snes.nesdev.org/wiki/PPU_registers), [PPU memory access](https://snes.nesdev.org/wiki/Reading_and_writing_PPU_memory) | register tests and golden framebuffer comparisons |
| DMA / HDMA | [DMA registers](https://snes.nesdev.org/wiki/DMA_registers) | transfer-pattern, arbitration, and scanline tests |
| ROM test corpus | [PeterLemon SNES tests](https://github.com/PeterLemon/SNES/tree/master/CPUTest), [higan SNES test ROMs](https://gitlab.com/higan/snes-test-roms), [SNESdev emulator tests](https://snes.nesdev.org/wiki/Emulator_tests) | pinned, opt-in ROM runs |

## Test policy

Normal `cargo test --workspace` must remain runnable without downloading ROMs.
External ROMs are pinned in `tests/snes-test-roms/sources.json`, fetched only
through the opt-in script, and never required by ordinary tests.

## Visual acceptance contract

PPU tests are not considered passing merely because a ROM reaches a stop state.
Each visual test must record:

- the immutable ROM source and revision;
- the frame number captured after reset and input setup;
- output dimensions and pixel format;
- a lossless reference image where possible;
- an explicit per-pixel tolerance and any masked unstable region.

PNG references can be strict golden tests after the renderer is deterministic.
JPEG references and tests involving documented hardware glitches remain
diagnostic until a tolerance/mask is recorded. A visual mismatch must preserve
the actual frame and a diff image as test artifacts for review.

## Current implementation boundary

Implemented and covered by ordinary workspace tests:

- 65816 state, a broad immediate/direct/absolute/long/indirect execution slice,
  stack/control flow, decimal ALU operations, block moves, interrupts, and
  ordered bus traces;
- LoROM/HiROM loading, copier-header stripping, SRAM windows, WRAM mirrors,
  controller serial input, general DMA, and full-state rewind checkpoints;
- PPU register latches, VRAM/CGRAM/OAM storage, a deterministic Mode 0 BG1
  renderer, bounded frame execution, and tolerant frame comparison;
- APU port/DSP-register state and an initial SPC700 execution subset.

Remaining before claiming broad emulator compatibility: complete 65816 and
SPC700 opcode coverage, scanline-level timing and HDMA, sprites/windows/color
math/Mode 7, DSP voice mixing, enhancement-chip cartridges, and pinned ROM
expected-result plus visual golden runs.
