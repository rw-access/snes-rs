# Accuracy ledger

This document records the sources of truth that future implementations should
use. The initial scaffold does not implement hardware behavior.

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
External ROMs belong under `tests/snes-test-roms/` only when a later task pins a
specific corpus and documents its source and revision.
