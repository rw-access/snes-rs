#!/usr/bin/env python3
"""Fetch pinned external SNES test sources and materialize a focused corpus.

Normal Cargo commands never invoke this script and therefore never require
network access. The script uses shallow bare Git caches and archives only the
paths/extensions declared in tests/snes-test-roms/sources.json.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile
import tarfile
from typing import Any


SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
DEFAULT_MANIFEST = Path("tests/snes-test-roms/sources.json")
DEFAULT_DESTINATION = Path("tests/snes-test-roms/materialized")
DEFAULT_CACHE = Path("tests/snes-test-roms/.git-cache")


class CorpusError(RuntimeError):
    """A user-actionable corpus setup error."""


def run_git(*args: str, cwd: Path | None = None, stdout: Any = None) -> None:
    command = ["git", *args]
    try:
        subprocess.run(command, cwd=cwd, check=True, stdout=stdout)
    except FileNotFoundError as exc:
        raise CorpusError("git is required to fetch the external test corpus") from exc
    except subprocess.CalledProcessError as exc:
        rendered = " ".join(command)
        raise CorpusError(f"command failed ({exc.returncode}): {rendered}") from exc


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise CorpusError(f"cannot read manifest {path}: {exc}") from exc

    if not isinstance(document, dict) or document.get("format_version") != 1:
        raise CorpusError(f"{path} must contain format_version = 1")
    sources = document.get("sources")
    if not isinstance(sources, list) or not sources:
        raise CorpusError(f"{path} must contain a non-empty sources array")

    seen: set[str] = set()
    for source in sources:
        if not isinstance(source, dict):
            raise CorpusError("each manifest source must be an object")
        source_id = source.get("id")
        repository = source.get("repository")
        revision = source.get("revision")
        paths = source.get("paths")
        extensions = source.get("include_extensions")
        if not isinstance(source_id, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", source_id):
            raise CorpusError(f"invalid source id: {source_id!r}")
        if source_id in seen:
            raise CorpusError(f"duplicate source id: {source_id}")
        seen.add(source_id)
        if not isinstance(repository, str) or not repository.startswith(("https://", "ssh://", "git@")):
            raise CorpusError(f"invalid repository URL for {source_id}")
        if not isinstance(revision, str) or not SHA1_RE.fullmatch(revision):
            raise CorpusError(f"{source_id}.revision must be a 40-character lowercase SHA-1")
        if not isinstance(paths, list) or not paths or not all(isinstance(item, str) and item for item in paths):
            raise CorpusError(f"{source_id}.paths must be a non-empty string array")
        if not isinstance(extensions, list) or not all(isinstance(item, str) and item.startswith(".") for item in extensions):
            raise CorpusError(f"{source_id}.include_extensions must be a dot-prefixed string array")

    return document


def cache_repository(source: dict[str, Any], cache_root: Path) -> Path:
    source_id = source["id"]
    repository = source["repository"]
    revision = source["revision"]
    cache = cache_root / f"{source_id}.git"
    cache_root.mkdir(parents=True, exist_ok=True)

    if not (cache / "HEAD").exists():
        if cache.exists() and any(cache.iterdir()):
            raise CorpusError(f"cache path is not an empty bare repository: {cache}")
        run_git("init", "--bare", str(cache))

    try:
        configured_url = subprocess.check_output(
            ["git", "-C", str(cache), "remote", "get-url", "origin"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        configured_url = ""
    if configured_url:
        if configured_url != repository:
            run_git("-C", str(cache), "remote", "set-url", "origin", repository)
    else:
        run_git("-C", str(cache), "remote", "add", "origin", repository)

    run_git("-C", str(cache), "fetch", "--depth=1", "origin", revision)
    try:
        resolved = subprocess.check_output(
            ["git", "-C", str(cache), "rev-parse", f"{revision}^{{commit}}"],
            text=True,
        ).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        raise CorpusError(f"pinned revision is unavailable after fetch: {source_id} {revision}") from exc
    if resolved != revision:
        raise CorpusError(f"revision mismatch for {source_id}: expected {revision}, got {resolved}")
    return cache


def is_documentation(member_name: str) -> bool:
    basename = PurePosixPath(member_name).name.lower()
    return basename.startswith(("readme", "license", "copying", "notice"))


def is_safe_member(member: tarfile.TarInfo) -> bool:
    path = PurePosixPath(member.name)
    return not path.is_absolute() and ".." not in path.parts and not (member.issym() or member.islnk())


def extract_archive(archive: Path, destination: Path, extensions: set[str]) -> int:
    extracted = 0
    with tarfile.open(archive, mode="r") as tar:
        for member in tar.getmembers():
            if not is_safe_member(member):
                raise CorpusError(f"refusing unsafe archive member: {member.name}")
            if member.isdir():
                continue
            suffix = PurePosixPath(member.name).suffix.lower()
            if suffix not in extensions and not is_documentation(member.name):
                continue
            target = destination / Path(*PurePosixPath(member.name).parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            source = tar.extractfile(member)
            if source is None:
                continue
            with target.open("wb") as output:
                shutil.copyfileobj(source, output)
            extracted += 1
    return extracted


def materialize_source(source: dict[str, Any], cache: Path, staging: Path) -> int:
    source_id = source["id"]
    archive = staging / f"{source_id}.tar"
    with archive.open("wb") as output:
        run_git(
            "-C",
            str(cache),
            "archive",
            "--format=tar",
            f"--prefix={source_id}/",
            source["revision"],
            "--",
            *source["paths"],
            stdout=output,
        )
    extensions = {extension.lower() for extension in source["include_extensions"]}
    return extract_archive(archive, staging / "corpus", extensions)


def safe_replace(destination: Path, repository_root: Path) -> None:
    resolved = destination.resolve()
    root = repository_root.resolve()
    corpus_root = (root / "tests" / "snes-test-roms").resolve()
    try:
        within_repo = os.path.commonpath((str(root), str(resolved))) == str(root)
        within_corpus = os.path.commonpath((str(corpus_root), str(resolved))) == str(corpus_root)
    except ValueError:
        within_repo = within_corpus = False
    if not within_repo or not within_corpus or resolved == corpus_root:
        raise CorpusError("--replace may only remove a child of tests/snes-test-roms")
    if destination.is_dir():
        shutil.rmtree(destination)
    elif destination.exists():
        destination.unlink()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--destination", type=Path, default=DEFAULT_DESTINATION)
    parser.add_argument("--cache-dir", type=Path, default=DEFAULT_CACHE)
    parser.add_argument("--source", action="append", dest="source_ids", help="fetch only this manifest source (repeatable)")
    parser.add_argument("--replace", action="store_true", help="replace an existing generated destination")
    parser.add_argument("--verify-only", action="store_true", help="validate metadata without contacting Git remotes")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest_path = args.manifest.resolve()
    document = load_manifest(manifest_path)
    sources = document["sources"]
    selected = set(args.source_ids or (source["id"] for source in sources))
    known = {source["id"] for source in sources}
    unknown = selected - known
    if unknown:
        raise CorpusError(f"unknown source id(s): {', '.join(sorted(unknown))}")
    if args.verify_only:
        print(f"validated {len(sources)} pinned sources in {manifest_path}")
        return 0

    repository_root = Path(__file__).resolve().parents[1]
    destination = (repository_root / args.destination).resolve() if not args.destination.is_absolute() else args.destination.resolve()
    cache_root = (repository_root / args.cache_dir).resolve() if not args.cache_dir.is_absolute() else args.cache_dir.resolve()
    if destination.exists() and not args.replace:
        raise CorpusError(f"destination already exists: {destination}; remove it or pass --replace")
    if destination.exists() and args.replace and args.source_ids:
        raise CorpusError("--replace with --source would discard unselected sources; use a separate destination")

    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".snes-test-roms-", dir=destination.parent) as temporary:
        staging = Path(temporary)
        total = 0
        for source in sources:
            if source["id"] not in selected:
                continue
            cache = cache_repository(source, cache_root)
            count = materialize_source(source, cache, staging)
            if count == 0:
                raise CorpusError(f"no selected files were found for {source['id']}")
            total += count
            print(f"materialized {source['id']}: {count} files")
        generated = staging / "corpus"
        if args.replace and destination.exists():
            safe_replace(destination, repository_root)
        generated.rename(destination)
    print(f"materialized {total} files under {destination}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except CorpusError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2)
