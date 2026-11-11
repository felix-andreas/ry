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
    just prune --tests       also remove test executables (each relinks at its next run)
    just prune --dry-run     only report what would go
"""

import argparse
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
    for profile in sorted(path for path in target.iterdir() if (path / "build").is_dir()):
        superseded = superseded_units(profile / "build")
        incremental = [path for path in (profile / "incremental").glob("*") if old_enough(path)]
        tests = test_executables(profile / "build") if arguments.tests else []
        for label, paths in [("unused units", superseded), ("incremental caches", incremental), ("test executables", tests)]:
            if paths:
                size = sum(map(disk_size, paths))
                print(f"{profile.name}: {len(paths)} {label}, {size / 1e9:.1f} GB")
                doomed += paths
    if arguments.dry_run:
        return
    for path in doomed:
        if path.is_dir():
            shutil.rmtree(path)
        else:
            path.unlink()
    freed = (shutil.disk_usage(target).free - before) / 1e9
    print(f"freed {freed:.1f} GB; {shutil.disk_usage(target).free / 1e9:.1f} GB free")


def superseded_units(build):
    """Every unit directory nothing has read for two days that has a sibling of the same package and kind."""
    doomed = []
    now = time.time()
    for package in build.iterdir():
        groups = {}
        for unit in package.iterdir():
            # The fingerprint's `.json` names the unit's kind: `lib-polars_ops.json`,
            # `bin-ry.json`, `test-integration-test-test_arrays.json`,
            # `run-build-script-build-script-build.json`.
            kind = tuple(sorted(path.name for path in (unit / "fingerprint").glob("*.json")))
            if kind:
                groups.setdefault(kind, []).append((last_used(unit), unit))
        for units in groups.values():
            units.sort()
            doomed += [unit for used, unit in units[:-1] if now - used > unused_seconds]
    return doomed


def test_executables(build):
    found = []
    for fingerprint in build.glob("*/*/fingerprint/test-*.json"):
        out = fingerprint.parent.parent / "out"
        found += [path for path in out.iterdir() if path.is_file() and os.access(path, os.X_OK) and old_enough(path)]
    return found


def last_used(unit):
    """The latest read or write of any file in the unit: a dependent's compile or link reads its rlib."""
    times = [path.stat() for path in unit.rglob("*") if path.is_file() and not path.is_symlink()]
    return max((max(stat.st_atime, stat.st_mtime) for stat in times), default=0)


def old_enough(path):
    return time.time() - path.stat().st_mtime > min_age_seconds


def disk_size(path):
    if path.is_file():
        return path.stat().st_blocks * 512
    return sum(child.stat().st_blocks * 512 for child in path.rglob("*") if not child.is_symlink())


if __name__ == "__main__":
    main()
