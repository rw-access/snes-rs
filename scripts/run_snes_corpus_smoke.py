#!/usr/bin/env python3
"""Run a reproducible bounded smoke inventory over materialized SNES ROMs.

This command reports whether each candidate ROM can be loaded and run for a
bounded number of master ticks.  A supported bounded run is not a correctness
pass; correctness still requires an explicit acceptance oracle.  Known
unsupported cartridge formats are reported separately so they do not get
silently counted as emulator successes.
"""

from __future__ import annotations

import argparse
from collections import Counter
from dataclasses import dataclass
import json
from pathlib import Path
import subprocess
import sys
from typing import Iterable


PROJECT_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ROOT = PROJECT_ROOT / "tests/snes-test-roms/materialized"
DEFAULT_RUNNER = PROJECT_ROOT / "target/debug/snes-test"
DEFAULT_EXPECTED = PROJECT_ROOT / "tests/snes-test-roms/smoke.json"
DEFAULT_SOURCES = PROJECT_ROOT / "tests/snes-test-roms/sources.json"
ROM_EXTENSIONS = (".sfc", ".smc")
UNSUPPORTED_MARKERS = (
    ("unsupported-coprocessor", "UnsupportedCoprocessor"),
    ("unsupported-format", "UnsupportedFormat"),
)


@dataclass(frozen=True)
class RomOutcome:
    path: Path
    category: str
    detail: str


def candidate_paths(root: Path) -> list[Path]:
    if not root.is_dir():
        raise ValueError(f"corpus root does not exist: {root}")
    return sorted(
        path
        for path in root.rglob("*")
        if path.is_file() and path.suffix.lower() in ROM_EXTENSIONS
    )


def classify(returncode: int, stderr: str) -> str:
    if returncode == 0:
        return "supported"
    for category, marker in UNSUPPORTED_MARKERS:
        if marker in stderr:
            return category
    return "unexpected"


def run_rom(runner: Path, rom: Path, ticks: int) -> RomOutcome:
    completed = subprocess.run(
        [str(runner), str(rom), str(ticks)],
        capture_output=True,
        text=True,
        check=False,
    )
    detail = (completed.stderr.strip() or completed.stdout.strip()).replace("\n", " ")
    return RomOutcome(rom, classify(completed.returncode, completed.stderr), detail)


def source_revisions(path: Path) -> dict[str, str]:
    document = json.loads(path.read_text(encoding="utf-8"))
    return {source["id"]: source["revision"] for source in document["sources"]}


def counts(outcomes: Iterable[RomOutcome]) -> dict[str, int]:
    result = Counter(outcome.category for outcome in outcomes)
    return {
        "candidates": sum(result.values()),
        "supported": result.get("supported", 0),
        "unsupported-coprocessor": result.get("unsupported-coprocessor", 0),
        "unsupported-format": result.get("unsupported-format", 0),
        "unexpected": result.get("unexpected", 0),
    }


def compare_expected(actual: dict[str, object], expected_path: Path) -> list[str]:
    expected = json.loads(expected_path.read_text(encoding="utf-8"))
    mismatches = []
    for key in ("format_version", "master_ticks", "rom_extensions", "source_revisions", "counts"):
        if actual.get(key) != expected.get(key):
            mismatches.append(f"{key}: expected {expected.get(key)!r}, got {actual.get(key)!r}")
    return mismatches


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT, help="materialized corpus root")
    parser.add_argument("--runner", type=Path, default=DEFAULT_RUNNER, help="bounded snes-test executable")
    parser.add_argument("--ticks", type=int, default=100_000, help="master-tick bound per ROM")
    parser.add_argument("--sources", type=Path, default=DEFAULT_SOURCES, help="pinned source manifest")
    parser.add_argument("--expected", type=Path, default=DEFAULT_EXPECTED, help="checked-in inventory baseline")
    parser.add_argument("--no-expected", action="store_true", help="skip comparison with the baseline")
    parser.add_argument("--report", type=Path, help="write the machine-readable report to this path")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.ticks <= 0:
        print("error: --ticks must be positive", file=sys.stderr)
        return 2
    if not args.runner.is_file():
        print(f"error: runner does not exist: {args.runner}", file=sys.stderr)
        return 2

    try:
        paths = candidate_paths(args.root)
        outcomes = [run_rom(args.runner, path, args.ticks) for path in paths]
        report: dict[str, object] = {
            "format_version": 1,
            "master_ticks": args.ticks,
            "rom_extensions": list(ROM_EXTENSIONS),
            "source_revisions": source_revisions(args.sources),
            "counts": counts(outcomes),
        }
        if args.report is not None:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    except (OSError, ValueError, json.JSONDecodeError, KeyError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    report_counts = report["counts"]
    assert isinstance(report_counts, dict)
    print(
        "snes corpus smoke: "
        f"candidates={report_counts['candidates']} "
        f"supported-bounded-runs={report_counts['supported']} "
        f"unsupported-coprocessor={report_counts['unsupported-coprocessor']} "
        f"unsupported-format={report_counts['unsupported-format']} "
        f"unexpected={report_counts['unexpected']} "
        f"ticks={args.ticks}"
    )

    mismatches = []
    if not args.no_expected:
        if not args.expected.is_file():
            mismatches.append(f"expected baseline does not exist: {args.expected}")
        else:
            try:
                mismatches = compare_expected(report, args.expected)
            except (OSError, ValueError, json.JSONDecodeError, KeyError) as error:
                mismatches.append(f"could not read expected baseline: {error}")
    if mismatches:
        print("inventory mismatch:", file=sys.stderr)
        for mismatch in mismatches:
            print(f"  {mismatch}", file=sys.stderr)

    unexpected = [outcome for outcome in outcomes if outcome.category == "unexpected"]
    for outcome in unexpected:
        print(f"unexpected: {outcome.path}: {outcome.detail}", file=sys.stderr)
    return 1 if mismatches or unexpected else 0


if __name__ == "__main__":
    raise SystemExit(main())
