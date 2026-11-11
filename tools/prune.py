"""Frees disk in cargo's target directory without forcing a full rebuild: `just prune`.

Cargo's build directory keeps one directory per compilation unit, `<profile>/build/<package>/<hash>`,
whose hash changes with the unit's configuration (profile settings, features, RUSTFLAGS, a
dependency's version, a `CARGO_PROFILE_*` override). A configuration that is no longer built stays
on disk forever, and in a 40 GB cloud container a few of them fill it: polars alone is gigabytes per
configuration. This removes a unit only when another unit of the same package and kind exists and
nothing has read its artifacts for two days, so a configuration that is still in use, such as the
one a test build alternates with, survives. It also removes incremental caches nothing has written
in the last hour (`--min-age`), which costs one full recompile of that crate at its next edit.

Use is read from access times. The usual `relatime` mount records the first read after a write and
then at most one read a day, which is why the threshold is two days: an access time two days old
means no read in the last day.

    just prune               remove unused units and stale incremental caches
    just prune --tests       also remove test executables (each recompiles at its next run)
    just prune --dry-run     only report what would go
"""

import argparse
import json
import os
import pathlib
import shutil
import sys
import time

root = pathlib.Path(__file__).resolve().parent.parent
min_age_seconds = 3600
unused_seconds = 2 * 86400


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tests", action="store_true", help="also remove test executables")
    parser.add_argument("--dry-run", action="store_true", help="report without deleting")
    parser.add_argument("--min-age", type=float, default=60, metavar="MINUTES", help="spare anything written more recently (default 60)")
    arguments = parser.parse_args()
    global min_age_seconds
    min_age_seconds = arguments.min_age * 60
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    if not target.is_dir():
        sys.exit(f"no target directory at {target}")
    before = shutil.disk_usage(target).free
    doomed = []
    for profile in sorted(path for path in directories(target) if (path / "build").is_dir()):
        superseded = superseded_units(profile / "build")
        incremental = [path for path in directories(profile / "incremental") if old_enough(path)]
        tests = test_executables(profile / "build") if arguments.tests else []
        for label, paths in [("unused units", superseded), ("incremental caches", incremental), ("test executables", tests)]:
            if paths:
                size = sum(map(disk_size, paths))
                print(f"{profile.name}: {len(paths)} {label}, {size / 1e9:.1f} GB")
                doomed += paths
    if arguments.dry_run:
        return
    for path in doomed:
        # A build running alongside may remove a file first.
        try:
            if path.is_dir():
                shutil.rmtree(path)
            else:
                path.unlink()
        except FileNotFoundError:
            pass
    freed = (shutil.disk_usage(target).free - before) / 1e9
    print(f"freed {freed:.1f} GB; {shutil.disk_usage(target).free / 1e9:.1f} GB free")


def superseded_units(build):
    """Every unit directory nothing has read for two days whose role another unit has taken over."""
    doomed = []
    now = time.time()
    for package in directories(build):
        groups = {}
        for unit in directories(package):
            role = unit_role(unit)
            if role:
                groups.setdefault(role, []).append((last_used(unit), unit))
        for units in groups.values():
            units.sort()
            doomed += [unit for used, unit in units[:-1] if now - used > unused_seconds]
    return doomed


def unit_role(unit):
    """What a unit is, leaving out how it was configured.

    The fingerprint's `.json` file names the unit's kind (`lib-polars_ops.json`, `bin-ry.json`,
    `run-build-script-build-script-build.json`), and its fields tell apart the units of one package
    that are all live: two versions of a crate (`path` hashes the source location, so syn 2 and
    syn 3 differ), a host and a target build (`compile_kind`), and two feature sets such as a
    build-dependency's and a normal dependency's. What is left to differ is the profile, the
    rustflags, the cargo config and the compiler, and a unit that differs only there is a
    configuration that has been replaced.
    """
    fingerprints = sorted(file_paths((unit / "fingerprint").glob("*.json")))
    if not fingerprints:
        return None
    try:
        fields = json.loads(fingerprints[0].read_text())
    except (OSError, ValueError):
        return None
    return (
        tuple(path.name for path in fingerprints),
        *(str(fields.get(key)) for key in ["path", "target", "compile_kind", "features"]),
    )


def test_executables(build):
    found = []
    for fingerprint in build.glob("*/*/fingerprint/test-*.json"):
        out = fingerprint.parent.parent / "out"
        found += [path for path in file_paths(out.glob("*")) if os.access(path, os.X_OK) and old_enough(path)]
    return found


def directories(path):
    return [child for child in path.iterdir() if child.is_dir() and not child.is_symlink()] if path.is_dir() else []


def file_paths(paths):
    return [path for path in paths if path.is_file() and not path.is_symlink()]


def last_used(unit):
    """The latest read or write of any file in the unit: a dependent's compile or link reads its rlib."""
    latest = 0
    for path in file_paths(unit.rglob("*")):
        try:
            stat = path.stat()
        except FileNotFoundError:
            continue
        latest = max(latest, stat.st_atime, stat.st_mtime)
    return latest


def old_enough(path):
    try:
        return time.time() - path.stat().st_mtime > min_age_seconds
    except FileNotFoundError:
        return False


def disk_size(path):
    total = 0
    for child in [path] if path.is_file() else file_paths(path.rglob("*")):
        try:
            total += child.stat().st_blocks * 512
        except FileNotFoundError:
            pass
    return total


if __name__ == "__main__":
    main()
