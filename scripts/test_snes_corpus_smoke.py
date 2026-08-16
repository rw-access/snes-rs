#!/usr/bin/env python3
"""Unit tests for the corpus smoke inventory classifier."""

from __future__ import annotations

import json
from pathlib import Path
import tempfile
import unittest

from run_snes_corpus_smoke import candidate_paths, classify, compare_expected, counts, RomOutcome


class CorpusSmokeTests(unittest.TestCase):
    def test_classifies_success_and_known_unsupported_load_errors(self) -> None:
        self.assertEqual(classify(0, ""), "supported")
        self.assertEqual(classify(1, "could not load: UnsupportedCoprocessor"), "unsupported-coprocessor")
        self.assertEqual(classify(1, "could not load: UnsupportedFormat"), "unsupported-format")

    def test_unknown_failure_is_not_counted_as_supported(self) -> None:
        self.assertEqual(classify(1, "panic: unexpected failure"), "unexpected")

    def test_candidate_inventory_is_sorted_and_only_includes_snes_roms(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "z.smc").write_bytes(b"")
            (root / "nested").mkdir()
            (root / "nested" / "a.SFC").write_bytes(b"")
            (root / "ignored.png").write_bytes(b"")
            self.assertEqual(
                [path.relative_to(root).as_posix() for path in candidate_paths(root)],
                ["nested/a.SFC", "z.smc"],
            )

    def test_counts_keep_supported_and_unsupported_separate(self) -> None:
        outcomes = [
            RomOutcome(Path("a.sfc"), "supported", ""),
            RomOutcome(Path("b.sfc"), "unsupported-coprocessor", ""),
            RomOutcome(Path("c.smc"), "unsupported-format", ""),
            RomOutcome(Path("d.sfc"), "unexpected", ""),
        ]
        self.assertEqual(
            counts(outcomes),
            {
                "candidates": 4,
                "supported": 1,
                "unsupported-coprocessor": 1,
                "unsupported-format": 1,
                "unexpected": 1,
            },
        )

    def test_expected_baseline_detects_drift(self) -> None:
        actual = {
            "format_version": 1,
            "master_ticks": 100,
            "rom_extensions": [".sfc", ".smc"],
            "source_revisions": {"example": "a" * 40},
            "counts": {"candidates": 1, "supported": 1, "unsupported-coprocessor": 0, "unsupported-format": 0, "unexpected": 0},
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "expected.json"
            path.write_text(json.dumps(actual), encoding="utf-8")
            self.assertEqual(compare_expected(actual, path), [])
            changed = dict(actual)
            changed["counts"] = dict(actual["counts"])
            changed["counts"]["supported"] = 0
            self.assertTrue(compare_expected(changed, path))


if __name__ == "__main__":
    unittest.main()
