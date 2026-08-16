# snes-rs

An instruction-oriented, dependency-light SNES emulator in Rust.

The repository is currently at the project-scaffold milestone. The workspace
establishes the frontend-neutral `snes-core`, headless `snes-render`, and
optional `snes-play` crate boundaries. CPU, PPU, cartridge loading, timing,
and rendering behavior will be added in later milestones.

## Workspace

- `crates/snes-core` — machine state and subsystem API boundaries.
- `crates/snes-render` — headless runner and rendering/test boundaries.
- `crates/snes-play` — future desktop frontend entry point.
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

ROM integration tests will remain opt-in once the runner and cartridge loader
exist; see `tests/snes-test-roms/README.md` for provenance and licensing notes.
