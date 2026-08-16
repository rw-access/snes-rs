# snes-rs

An instruction-oriented, dependency-light SNES emulator in Rust.

The workspace contains a working frontend-neutral core and headless validation
tools. The current NTSC baseline includes LoROM/HiROM loading, 65816 CPU
execution, WRAM/SRAM, controllers, DMA/HDMA, scanline/VBlank scheduling,
rewind snapshots, PPU modes 0–7 (with bounded high-resolution/direct-color
paths), SPC700/APU I/O, and a bounded BRR/DSP mixer.

## Workspace

- `crates/snes-core` — machine state, CPU, cartridge, PPU, DMA, APU, and
  rewind APIs.
- `crates/snes-render` — headless ROM execution, conformance, and visual
  acceptance tools.
- `crates/snes-play` — dependency-light bounded-run CLI with optional PPM
  framebuffer output.
- `docs/accuracy.md` — authoritative reference ledger.
- `tests/` — local fixtures and the documented opt-in ROM corpus location.

## Development

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
```

The pinned external corpus is optional and never required by ordinary Cargo
commands. Verify its metadata or fetch the focused local corpus explicitly:

```sh
python3 scripts/fetch_snes_test_roms.py --verify-only
python3 scripts/fetch_snes_test_roms.py
```

Run a bounded ROM smoke test:

```sh
cargo run -p snes-render --bin snes-test -- path/to/game.sfc 1000000
```

Run the integrated CLI frontend and capture a frame:

```sh
cargo run -p snes-play -- path/to/game.sfc --frames 1 --ppm frame.ppm
```

Run the opt-in 65816 vector harness against a materialized SingleStepTests
file:

```sh
cargo run -p snes-render --bin snes-65816-conformance -- \
  tests/snes-test-roms/materialized/singlesteptests-65816/v1/69.e.json \
  --limit 10000
```

Visual ROM cases are driven by `tests/snes-test-roms/cases.toml`:

```sh
cargo run -p snes-render --bin snes-acceptance -- \
  --manifest tests/snes-test-roms/cases.toml --case peterlemon-cpuadc
```

The pinned corpus and its provenance rules are documented in
`tests/snes-test-roms/README.md`. Ordinary Cargo tests do not download or
require ROMs. Bounded execution is diagnostic unless a case has a verified
memory, register, timing, audio, or framebuffer oracle; the current visual
examples remain diagnostic for that reason.
