# ADR 0001: Initial Architecture Contracts

- Status: Accepted
- Date: 2026-08-15
- Scope: Batch 1 public architecture and state contracts

## Decision

1. `Console` is the canonical public machine façade. It owns the complete
   mutable SNES machine state and exposes construction, reset, execution,
   frame, memory-inspection, and snapshot operations. `SnesCore` is not a
   second public ownership type; code that needs an internal name may use
   private implementation details without creating a competing façade.

2. Normal instruction execution returns a compact `CpuStep` summary and does
   not allocate a bus-access vector. The CPU and bus produce accesses through
   one ordered execution plan. An optional trace sink receives each
   `BusAccess` in order; tests and diagnostic tools may provide a collecting
   sink backed by `Vec`. Tracing is therefore an opt-in allocation and has no
   required cost in the normal path.

3. `BusAccess` is the ordered logical record of an opcode fetch, operand fetch,
   data access, dummy access, vector fetch, or internal slot. `CpuStep.cycles`
   reports logical 5A22 execution cycles, while `CpuStep.master_ticks` reports
   the elapsed NTSC master-clock timeline. `master_ticks` is the authoritative
   quantity for scheduling and synchronization; it is not computed from the
   number of accesses, and no fixed cycles-to-master-ticks ratio is assumed.

   The CPU execution plan declares access order and internal work. The
   bus/timing domain assigns the master-clock cost using the access region,
   access kind, and current timing state, and advances the shared timeline.
   Peripheral scheduling, DMA/HDMA, interrupts, and future PPU synchronization
   consume that same timeline. `cycles` and the ordered trace are still
   deterministic observables and must be identical between traced and
   untraced execution.

4. `RomImage` is immutable, normalized loader output: ROM bytes plus validated
   metadata and a stable identity. `Console` transfers it to a boxed
   `CartridgeDevice`, which owns mapper state, mutable SRAM, and any
   cartridge-side registers. Cartridge writes never mutate `RomImage`.
   `CartridgeDevice` is the extension point for LoROM, HiROM, SRAM variants,
   and later enhancement chips; mapper selection belongs in cartridge loading,
   not in CPU or console-bus logic. Cartridge state participates in snapshots.

5. A snapshot is a complete, versioned, deterministic value of all mutable
   console state required to continue execution: CPU mode/register state,
   memory, PPU/peripheral registers and latches, pending events, cartridge
   mapper/SRAM state, controller state, and timing counters. It includes the
   ROM identity and relevant configuration so restoration can reject a
   different machine image. Immutable ROM bytes, host/frontend objects, and
   rebuildable caches are not duplicated in the snapshot.

   Snapshot encoding is platform-independent and deterministic. Given the
   same ROM, initial state, and input sequence, snapshots, traces, and frame
   outputs are byte-identical. Restoring a snapshot and replaying the same
   inputs must produce the same subsequent observables.

## Options considered

- Make `SnesCore` the public façade and use `Console` as an alias or wrapper.
- Keep both `Console` and `SnesCore` as public owners.
- Store `Vec<BusAccess>` in every `CpuStep`.
- Expose only a coarse step summary and make detailed tracing a separate CPU
  implementation.
- Treat CPU cycles, access count, or a fixed ratio as the timing authority.
- Let the CPU assign all access timing directly from opcode metadata.
- Put mutable SRAM and mapper state in `RomImage`.
- Model each mapper as a separate bus implementation with no common cartridge
  extension point.
- Snapshot only CPU registers and RAM, or serialize host/runtime objects as
  part of machine state.

## Chosen approach

Use one public `Console`, one semantic CPU execution plan, an optional
streaming trace sink, bus-owned master-clock accounting, immutable ROM input
plus stateful cartridge device, and a complete versioned machine snapshot.

## Reasoning

This preserves the plan's frontend-neutral API while avoiding duplicate
ownership models. It keeps ordinary execution allocation-free without giving
up bus-cycle evidence needed by SingleStepTests, DMA, interrupts, and later
PPU synchronization. The SNES has region-dependent access timing, so the bus
and shared timing domain must own elapsed master clocks rather than hiding a
false fixed conversion in opcode metadata. Separating immutable ROM data from
mapper/SRAM state makes loading, mapper extension, and save-state behavior
explicit. A complete value snapshot is the smallest contract that can support
deterministic replay and meaningful failure reproduction as more peripherals
become stateful.

## Consequences

- Public documentation and integration tests use `Console` exclusively.
- `CpuStep` remains cheap in normal execution; trace tests must explicitly
  install a collector or sink.
- Fast execution may batch work, but it must preserve the same step totals and
  trace ordering as traced execution.
- Timing tests assert master-clock deltas and event ordering, not just
  instruction counts.
- Cartridge implementations must expose serializable mapper/SRAM state and
  report access speed through the cartridge/timing boundary.
- Snapshot schemas require versioning and compatibility tests; adding a new
  mutable subsystem requires updating snapshot capture, restore, and replay
  tests.

## Rejected alternatives

- `SnesCore` as a second canonical façade: rejected because it duplicates the
  ownership model implied by the planned `Console` API and invites drift.
- Per-step `Vec<BusAccess>`: rejected for the normal path because it allocates
  on every instruction; collection remains available in trace mode.
- A separate trace execution engine: rejected because it can diverge from the
  path used by games and make conformance evidence unreliable.
- Fixed cycles/access-count timing: rejected because access speed depends on
  bus region and timing state.
- CPU-owned timing costs: rejected because it couples opcode semantics to the
  memory map and blocks coherent DMA/PPU scheduling.
- Mutable `RomImage`: rejected because it conflates immutable program data
  with cartridge runtime state and complicates reuse and identity checks.
- Partial or host-dependent snapshots: rejected because they cannot guarantee
  deterministic restore/replay across runs or platforms.

## Affected tasks

- Batch 1 scaffold: define `Console`, `CpuStep`, `BusAccess`, timing, ROM,
  cartridge, and snapshot contracts around this ADR.
- CPU and trace-bus work: emit one ordered access plan; support an optional
  trace sink and deterministic step summaries.
- Cartridge/ROM loader: normalize input into immutable `RomImage`, select a
  `CartridgeDevice`, and keep mapper/SRAM state serializable.
- Console bus and timing: own master-clock accounting and shared event
  scheduling; do not derive timing from access count.
- PPU/DMA/interrupt work: synchronize through the shared master-clock domain.
- Test infrastructure: add trace collectors, snapshot round-trip/replay
  tests, and assertions that traced and untraced execution agree.
- Documentation/review: use this ADR as the Batch 1 architecture gate before
  emulator behavior is implemented.
