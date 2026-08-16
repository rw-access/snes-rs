# Optional SNES test-ROM corpus

The repository tracks only [sources.json](sources.json), which records the
upstream URLs and immutable commits used for the initial corpus. ROMs, JSON
vectors, screenshots, and Git caches are generated locally and are ignored by
Git. Ordinary `cargo test --workspace` commands do not read this directory or
access the network.

## Pinned sources

- `SingleStepTests/65816` at
  `db6b10401729d5f20f2181dde5d3d7b037093a4a`: 65816 JSON vectors.
- `PeterLemon/SNES` at
  `350b394e86ec5d62f600b5cbf64cdce3721bb6ef`: compiled CPU test ROMs.
- `higan/snes-test-roms` at
  `26e8aa91e0d3b90b901e7e693bc4a8244b391d7b`: selected hardware-test ROMs,
  audio fixtures, documentation, and image captures.

The pins and selected subpaths are authoritative in `sources.json`; keep this
README descriptive so the manifest remains machine-readable.

## Fetch and materialize

From the repository root, run:

```text
python3 scripts/fetch_snes_test_roms.py --verify-only
python3 scripts/fetch_snes_test_roms.py
```

The second command shallow-fetches each pinned commit into
`tests/snes-test-roms/.git-cache/` and writes the focused corpus to
`tests/snes-test-roms/materialized/`. Fetching is explicit and opt-in. To
refresh an existing generated corpus after changing a pin or selected path,
run the command with `--replace`:

```text
python3 scripts/fetch_snes_test_roms.py --replace
```

Use `--source singlesteptests-65816`, `--source peterlemon-snes`, or
`--source higan-snes-test-roms` to materialize one source. `--manifest`,
`--destination`, and `--cache-dir` are available for CI or an external cache.

When refreshing pins, verify the full 40-character commit ID in the upstream
repository, update only `sources.json`, run `--verify-only`, and record the
reason/date in the change description. Do not replace an immutable commit with
a moving branch name.

## Provenance and legal assumptions

These upstream projects are public repositories, but their ROMs, screenshots,
source code, and fixtures may have different authors and licensing terms. The
manifest does not grant redistribution rights and makes no blanket license
claim. Keep generated artifacts local unless the applicable upstream terms and
any author permissions allow redistribution. Preserve the upstream README or
license files when using a fixture, and consult the source repository before
publishing test data or derivative golden images.

ROM execution is an opt-in integration concern (for example, a future runner
may require `SNES_RUN_ROM_TESTS=1`); the corpus fetch itself is never an
implicit test dependency.
