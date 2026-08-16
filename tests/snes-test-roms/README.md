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

`--verify-only` checks only the local manifest: it does not contact Git or
validate that the upstream commit is reachable. After a real fetch, verify the
resolved object when provenance matters, for example:

```sh
git -C tests/snes-test-roms/.git-cache/singlesteptests-65816.git \
  rev-parse db6b10401729d5f20f2181dde5d3d7b037093a4a^{commit}
git -C tests/snes-test-roms/.git-cache/peterlemon-snes.git \
  rev-parse 350b394e86ec5d62f600b5cbf64cdce3721bb6ef^{commit}
git -C tests/snes-test-roms/.git-cache/higan-snes-test-roms.git \
  rev-parse 26e8aa91e0d3b90b901e7e693bc4a8244b391d7b^{commit}
```

Each command must print exactly the revision in `sources.json`. A successful
fetch and a nonempty materialized directory establish corpus provenance only;
they do not establish emulator correctness.

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

## What can currently be called a pass

The corpus has three different kinds of evidence and must not be flattened into
one file-count or exit-status check:

| Material | Correct oracle |
| --- | --- |
| SingleStepTests `v1/*.json` | Initial/final CPU state, memory, and the applicable bus-access sequence. |
| PeterLemon CPU `.sfc` plus matching `.png` | The ROM's documented completion/result plus a captured frame compared with its matching lossless reference. |
| higan `.sfc`/`.smc` hardware tests | A case-specific memory, register, timing, audio, or visual oracle taken from the upstream documentation. |

The corpus also contains `.spc`, `.bin`, source, and standalone image files.
Those are fixtures for a future subsystem harness, not bootable SNES ROM cases.
Only files with an explicit descriptor should enter an automated suite.

The current diagnostic command is:

```sh
cargo run -p snes-render --bin snes-test -- \
  tests/snes-test-roms/materialized/<source>/<case>.sfc 100000
```

Its `ticks=... stopped=...` output proves only that loading and bounded
execution completed. `stopped=true` is not a correctness result, and
`stopped=false` is not a failure. The command does not capture a frame, compare
an image, inspect a result sentinel, or run the SingleStepTests vectors. The
limit is instruction/DMA-granular, so the reported tick count may exceed the
requested bound; a future gate must define the allowed overshoot. The current
CLI also does not provide a complete nonzero-on-failure result protocol, and
the runner's mapping descriptor is not yet an applied mapping override. Until
those oracles and protocols are implemented, no external-ROM pass claim should
be made.

## Visual case record

Every enabled visual case needs a small descriptor, kept with the acceptance
harness rather than inferred from filenames, containing:

- source ID, pinned revision, relative ROM path, and ROM checksum;
- reset/input setup and the exact frame number to capture;
- expected width, height, row-major pixel format, and reference-image path;
- per-channel tolerance and an explicit mask (or an explicit empty mask);
- whether the result is a strict gate or diagnostic-only, with the reason;
- the artifact path for actual and diff images on failure.

For deterministic PNG cases, the default gate is exact dimensions and zero
per-channel tolerance. Any relaxation must be justified in the descriptor.
JPEG references, converted captures, and documented glitch cases stay
diagnostic until their tolerance and mask are reproducible. A ROM reaching a
stop instruction is never a substitute for this frame comparison.
