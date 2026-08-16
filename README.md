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

After building the headless runner, reproduce the pinned corpus capability
inventory. This reports bounded execution separately from known unsupported
cartridge formats and checks the result against `tests/snes-test-roms/smoke.json`:

```sh
cargo build -p snes-render --bin snes-test
python3 scripts/run_snes_corpus_smoke.py \
  --report target/snes-corpus-smoke.json
```

The `supported-bounded-runs` count is not a correctness pass; only explicit
result or visual acceptance cases can establish correctness.

Run a bounded ROM smoke test:

```sh
cargo run -p snes-render --bin snes-test -- path/to/game.sfc 1000000
```

For a ROM with an explicitly documented memory-byte result, the runner can
enforce the result and return a failing exit status for running or failed
tests:

```sh
cargo run -p snes-render --bin snes-test -- path/to/test.sfc 1000000 \
  --result-address 0x000000 --result-running 0 --result-passed 1
```

Use `--mapping lorom` or `--mapping hirom` when authoritative test metadata
overrides automatic cartridge-header detection.

Run the integrated CLI frontend and capture a frame:

```sh
cargo run -p snes-play -- path/to/game.sfc --frames 1 --ppm frame.ppm
```

Replay an lsnes SNES movie (`.lsmv`) into an MP4. The movie contains one
controller sample per frame; the exporter also writes the emulator's stereo
audio into the MP4. Use `--sample-dir` to retain PPM frames for visual review
and `--audio-wav` to retain the generated PCM audio for inspection:

```sh
cargo run -p snes-render --bin lsmv_to_mp4 -- \
  path/to/movie.lsmv path/to/game.sfc \
  --output movie.mp4 --sample-dir movie-samples --sample-every 600 \
  --audio-wav movie.wav
```

The exporter verifies the LSMV's recorded ROM SHA-256 by default. Use
`--ignore-rom-checksum` only when exercising the movie plumbing against a
different test ROM.

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
memory, register, timing, audio, or framebuffer oracle. The 11 enabled
PeterLemon visual gates (10 PPU captures plus CPU MOV) are strict framebuffer
gates with exact matches, and the documented gilyon SPC700 ROM adds one strict
result-protocol gate; other visual examples remain diagnostic until their
oracle and modeled hardware are sufficient.
