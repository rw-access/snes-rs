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

## Corpus audit and acceptance tiers

The manifest is a provenance inventory, not a declaration that every materialized
file is a bootable SNES ROM or has an automatic oracle. The files must be
accepted according to their role:

| Corpus material | Acceptance role | Required evidence | Current status |
| --- | --- | --- | --- |
| `singlesteptests-65816/v1/*.json` | CPU conformance | Compare initial/final registers, memory, and applicable ordered bus accesses for the selected vectors. | Not wired into a repository test yet. |
| `peterlemon-snes/CPUTest/CPU/*.sfc` with its sibling `.png` | CPU/PPU integration | Run the matching ROM to its documented result, capture a declared frame, and compare the matching lossless reference. | The optional acceptance runner captures frames and compares PPM/ARGB32 references; the pinned PNG examples remain diagnostic. |
| Bootable `.sfc`/`.smc` files under `higan-snes-test-roms` | Hardware integration | A per-ROM descriptor must name the result oracle: memory/register state, timing trace, audio output, or frame comparison. | No generic pass rule exists; bounded execution is diagnostic only. |
| `.spc`, `.bin`, source files, and standalone image/audio fixtures | Subsystem fixture | Consume them only through the subsystem-specific harness that defines their format and oracle. | Must not be passed to the SNES boot-ROM runner as a suite. |

This prevents a successful file load, a non-crashing bounded run, or an early
`STP` from being reported as emulator compatibility.

The corpus gates are intentionally layered:

1. `python3 scripts/fetch_snes_test_roms.py --verify-only` validates the
   manifest shape and immutable revision strings. It does not contact the
   remotes and therefore does not prove that a revision is fetchable.
2. A real fetch must resolve every selected cache to the exact manifest commit
   and materialize at least one selected file. The cache's `rev-parse` result is
   the provenance check; file counts alone are not.
3. `cargo test --workspace` proves only the checked-in unit and boundary tests.
   It deliberately says nothing about the optional corpus.
4. An external-ROM gate may be called a pass only when it reports the named
   oracle for every selected case. The current `snes-test` binary reports only
   bounded ticks and `stopped`; it is not that gate.

The current execution limit is instruction/DMA-granular rather than a hard
cutoff: a request for `N` master ticks may report a value greater than `N` after
the final instruction or transfer. An acceptance descriptor records the bound,
but a strict timing gate must still define the allowed overshoot. The legacy
`snes-test` CLI prints bounded diagnostic output only; the opt-in
`snes-acceptance` binary provides nonzero results for strict case failures. The
`RomTestDescriptor::mapping` field is also not evidence of an override until
the runner actually applies it when loading a ROM.

## Visual acceptance contract

PPU tests are not considered passing merely because a ROM reaches a stop state.
Each visual test must record:

- the immutable ROM source and revision;
- the exact ROM path within that pinned source and a checksum of the input file;
- the frame number captured after reset and input setup;
- output dimensions and pixel format. Until the video API exposes this as a
  named type, the current convention is row-major `u32` pixels in `0xAARRGGBB`
  order, with the top-left pixel first;
- a lossless reference image where possible;
- an explicit per-channel tolerance and any masked unstable region, including
  the reason for every nonzero tolerance or mask.

For the ordinary NTSC baseline, a reference must normally be 256x224. A test
must still record its actual dimensions because hires, interlace, and external
captures may differ. PNG references can be strict golden tests only after the
renderer and frame boundary are deterministic; the default PNG tolerance is
zero. JPEG references, format-converted captures, and tests involving documented
hardware glitches remain diagnostic until a justified tolerance/mask is
recorded.

A visual gate must fail on a dimension, frame-selection, or pixel mismatch. The
acceptance runner preserves the actual frame and, on mismatch, a deterministic
diff image together with case metadata under
`target/snes-acceptance/<case>/`. The comparator supports an explicit
one-byte-per-pixel mask. The harness currently decodes binary P6 PPM and
little-endian ARGB32 references; PNG/JPEG corpus captures remain diagnostic
until a decoder is deliberately added, so no pinned visual ROM is currently an
enabled strict golden gate.

Before enabling one, the harness must prove determinism by running the same ROM
and input sequence twice from reset and comparing the captured frame and the
relevant machine-state/result oracle. `run_for_frames` plus `next_frame` is not
yet sufficient evidence by itself: the current frame duration is an approximate
bound and scanline/VBlank frame synchronization is still incomplete.

## Current implementation boundary

The following have focused ordinary workspace tests. That evidence is narrower
than corpus compatibility and must not be promoted to a ROM-suite pass:

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
math/Mode 7, DSP voice mixing, enhancement-chip cartridges, JSON-vector
execution, and pinned ROM expected-result plus visual golden runs. The
acceptance harness now has per-ROM descriptors, ROM/reference provenance,
bounded execution, frame capture, deterministic replay, and diff artifacts;
those mechanisms do not constitute a corpus pass until strict cases with valid
oracles are enabled and pass.
