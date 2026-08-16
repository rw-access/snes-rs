#!/usr/bin/env python3
"""Verify optional SNES acceptance descriptors without network access.

This checks that every case names a source revision present in sources.json,
uses safe relative paths, and matches local ROM/reference SHA-256 values when
the generated corpus is available. Missing generated files are allowed unless
--require-files is supplied.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import sys

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 and older; the descriptor is intentionally tiny.
    tomllib = None  # type: ignore[assignment]


SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


def safe_relative(value: str, field: str) -> Path:
    path = PurePosixPath(value)
    if not value or path.is_absolute() or ".." in path.parts:
        raise ValueError(f"{field} must be a non-empty relative path: {value!r}")
    return Path(*path.parts)


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def load_descriptor(path: Path) -> dict:
    text = path.read_text(encoding="utf-8")
    if tomllib is not None:
        return tomllib.loads(text)
    document: dict[str, object] = {}
    cases: dict[str, dict[str, object]] = {}
    document["case"] = cases
    current: dict[str, object] | None = None
    for raw_line in text.splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[case.") and line.endswith("]"):
            case_id = line[6:-1]
            current = {}
            cases[case_id] = current
            continue
        key, separator, raw_value = line.partition("=")
        if not separator:
            raise ValueError(f"invalid descriptor line: {raw_line!r}")
        key = key.strip()
        raw_value = raw_value.strip()
        if raw_value.startswith('"'):
            value: object = json.loads(raw_value)
        elif raw_value in ("true", "false"):
            value = raw_value == "true"
        else:
            value = int(raw_value, 0)
        (current if current is not None else document)[key] = value
    return document


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=Path("tests/snes-test-roms/cases.toml"))
    parser.add_argument("--sources", type=Path, default=Path("tests/snes-test-roms/sources.json"))
    parser.add_argument("--corpus-root", type=Path, help="override manifest corpus_root")
    parser.add_argument("--require-files", action="store_true", help="fail if a case file is not materialized")
    args = parser.parse_args()

    try:
        manifest = load_descriptor(args.manifest)
        sources_document = json.loads(args.sources.read_text(encoding="utf-8"))
        sources = {source["id"]: source for source in sources_document["sources"]}
        cases = manifest["case"]
        if not isinstance(cases, dict) or not cases:
            raise ValueError("manifest must contain at least one [case.<id>] section")
        corpus_root = args.corpus_root or Path(manifest["corpus_root"])
        checked = 0
        missing = 0
        for case_id, case in cases.items():
            source_id = case["source"]
            if source_id not in sources:
                raise ValueError(f"{case_id}: unknown source {source_id!r}")
            if case["revision"] != sources[source_id]["revision"]:
                raise ValueError(f"{case_id}: revision does not match sources.json")
            rom_sha = case["rom_sha256"]
            if not SHA256_RE.fullmatch(rom_sha):
                raise ValueError(f"{case_id}: rom_sha256 must be lowercase SHA-256")
            rom_path = safe_relative(case["rom"], f"{case_id}.rom")
            root = corpus_root / source_id
            rom = root / rom_path
            if rom.is_file():
                actual = digest(rom)
                if actual != rom_sha:
                    raise ValueError(f"{case_id}: ROM checksum mismatch ({actual})")
                checked += 1
            else:
                missing += 1
                if args.require_files:
                    raise ValueError(f"{case_id}: missing ROM {rom}")

            reference = case.get("reference")
            reference_sha = case.get("reference_sha256")
            if reference_sha is not None and not SHA256_RE.fullmatch(reference_sha):
                raise ValueError(f"{case_id}: reference_sha256 must be lowercase SHA-256")
            if reference is not None:
                reference_path = safe_relative(reference, f"{case_id}.reference")
                reference_file = root / reference_path
                if reference_file.is_file() and reference_sha is not None:
                    actual = digest(reference_file)
                    if actual != reference_sha:
                        raise ValueError(f"{case_id}: reference checksum mismatch ({actual})")
                elif args.require_files:
                    raise ValueError(f"{case_id}: missing reference {reference_file}")
    except (OSError, KeyError, TypeError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    print(f"validated {len(cases)} acceptance cases; local ROMs checked={checked} missing={missing}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
