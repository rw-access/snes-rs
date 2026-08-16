#!/usr/bin/env python3
"""Boot every materialized SNES ROM and retain a bounded title-screen capture."""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import asdict, dataclass
import hashlib
import json
from pathlib import Path
import subprocess
import time


PROJECT_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ROOT = PROJECT_ROOT / "tests/snes-test-roms/materialized"
DEFAULT_RUNNER = PROJECT_ROOT / "target/debug/snes-play"
DEFAULT_OUTPUT = PROJECT_ROOT / "artifacts/title-screen-pass.json"
ROM_EXTENSIONS = {".sfc", ".smc"}


@dataclass
class Outcome:
    rom: str
    category: str
    returncode: int | None
    elapsed_seconds: float
    screenshot: str | None
    sha256: str | None
    dimensions: str | None
    stdout: str
    stderr: str


def candidates(root: Path) -> list[Path]:
    return sorted(
        path for path in root.rglob("*") if path.is_file() and path.suffix.lower() in ROM_EXTENSIONS
    )


def ppm_dimensions(path: Path) -> str | None:
    try:
        with path.open("rb") as stream:
            if stream.readline().strip() != b"P6":
                return None
            dimensions = stream.readline().split()
            if len(dimensions) != 2:
                return None
            return f"{int(dimensions[0])}x{int(dimensions[1])}"
    except (OSError, ValueError):
        return None


def capture(root: Path, output_root: Path, runner: Path, rom: Path, frames: int, timeout: float) -> Outcome:
    relative = rom.relative_to(root)
    screenshot_path = output_root / relative.with_suffix(".ppm")
    screenshot_path.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    try:
        completed = subprocess.run(
            [str(runner), str(rom), "--frames", str(frames), "--ppm", str(screenshot_path)],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
        elapsed = time.monotonic() - started
        stdout = completed.stdout.strip()
        stderr = completed.stderr.strip()
        if completed.returncode != 0:
            category = "boot-failure"
            screenshot = None
            sha256 = None
            dimensions = None
        elif not screenshot_path.is_file():
            category = "capture-failure"
            screenshot = None
            sha256 = None
            dimensions = None
        else:
            category = "captured"
            screenshot = str(screenshot_path.relative_to(PROJECT_ROOT))
            sha256 = hashlib.sha256(screenshot_path.read_bytes()).hexdigest()
            dimensions = ppm_dimensions(screenshot_path)
        return Outcome(
            str(relative), category, completed.returncode, elapsed, screenshot, sha256,
            dimensions, stdout, stderr,
        )
    except subprocess.TimeoutExpired as error:
        return Outcome(
            str(relative), "timeout", None, time.monotonic() - started, None, None, None,
            (error.stdout or "").strip() if isinstance(error.stdout, str) else "",
            (error.stderr or "").strip() if isinstance(error.stderr, str) else "",
        )
    except OSError as error:
        return Outcome(
            str(relative), "runner-error", None, time.monotonic() - started, None, None, None,
            "", str(error),
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    parser.add_argument("--runner", type=Path, default=DEFAULT_RUNNER)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--frames", type=int, default=180)
    parser.add_argument("--workers", type=int, default=6)
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args()
    if args.frames <= 0 or args.workers <= 0 or args.timeout <= 0:
        parser.error("frames, workers, and timeout must be positive")
    if not args.root.is_dir():
        parser.error(f"ROM root does not exist: {args.root}")
    if not args.runner.is_file():
        parser.error(f"runner does not exist: {args.runner}")

    args.root = args.root.resolve()
    args.runner = args.runner.resolve()
    args.output = args.output.resolve()

    roms = candidates(args.root)
    output_root = args.output.parent / "title-screens"
    output_root.mkdir(parents=True, exist_ok=True)
    outcomes: list[Outcome] = []
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = {
            pool.submit(capture, args.root, output_root, args.runner, rom, args.frames, args.timeout): rom
            for rom in roms
        }
        for future in as_completed(futures):
            outcome = future.result()
            outcomes.append(outcome)
            print(f"{outcome.category}\t{outcome.rom}", flush=True)
    outcomes.sort(key=lambda outcome: outcome.rom)
    counts: dict[str, int] = {}
    for outcome in outcomes:
        counts[outcome.category] = counts.get(outcome.category, 0) + 1
    report = {
        "format_version": 1,
        "frames": args.frames,
        "workers": args.workers,
        "timeout_seconds": args.timeout,
        "candidate_count": len(roms),
        "counts": counts,
        "outcomes": [asdict(outcome) for outcome in outcomes],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"summary\t{json.dumps(counts, sort_keys=True)}")
    return 0 if counts.get("boot-failure", 0) == 0 and counts.get("timeout", 0) == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
