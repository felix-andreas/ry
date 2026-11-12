#!/usr/bin/env python3
"""Drops from the page cache every file an ry build maps at run time, so two builds compared on
peak RSS start from the same page-cache state.

A file mapping's resident size depends on how the page cache holds the file: a freshly written
`libR.so`, `.ryc` or start image sits in large folios, so one fault maps a whole block, while the
same file read back after eviction maps a few pages at a time. A build that was just compiled or
just ran the oracle therefore measures several MB more `RssFile` than an identical build that did
not, so evict both builds before comparing them.

The files are the executable's own directory, the base R tree `rcmd::base::installed` would find
for it (beside it, under `../lib/ry`, or in any directory up to the cargo target directory, or
`RY_BASE_R`), ry's cache directory with every extracted runtime and R home in it, the R home
`R_HOME` names (a separate home per build, made with `ry-rcmd start-image`, lives anywhere), and
the package libraries `R_LIBS` and `R_LIBS_USER` name. `R_HOME` may list several homes, separated
as a path list, so one call evicts every build's home. Each tree is walked recursively, following
symbolic links, since ry's cached R home links into the base R tree.

Usage: `python3 tools/bench/evict.py RY [RY ...]`, or `evict(paths)` from Python.
"""

import os
import pathlib
import sys


def evict(executables):
    """Evicts every file the ry builds at `executables` read at run time, and returns how many
    files it evicted."""
    seen = set()
    count = 0
    for root in runtime_roots(executables):
        for path in files_under(root):
            try:
                status = os.stat(path)
            except OSError:
                continue
            key = status.st_dev, status.st_ino
            if key in seen:
                continue
            seen.add(key)
            count += drop(path)
    return count


def runtime_roots(executables):
    roots = []
    for executable in executables:
        directory = pathlib.Path(executable).resolve().parent
        roots += [path for path in directory.iterdir() if path.is_file()]
        roots += [directory / "base-r", directory / "../lib/ry/base-r"]
        target = next((ancestor for ancestor in directory.parents if (ancestor / "CACHEDIR.TAG").is_file()), None)
        if target is not None:
            for ancestor in [directory, *directory.parents]:
                roots.append(ancestor / "base-r")
                if ancestor == target:
                    break
    cache = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    roots.append(pathlib.Path(cache) / "ry")
    for variable in ("RY_BASE_R", "R_HOME", "R_LIBS", "R_LIBS_USER"):
        roots += [pathlib.Path(part) for part in os.environ.get(variable, "").split(os.pathsep) if part]
    return [root for root in roots if root.exists()]


def files_under(root):
    if root.is_file():
        yield root
        return
    # A symbolic link back up the tree would make a walk that follows links loop.
    visited = set()
    for directory, subdirectories, names in os.walk(root, followlinks=True):
        status = os.stat(directory)
        if (status.st_dev, status.st_ino) in visited:
            subdirectories.clear()
            continue
        visited.add((status.st_dev, status.st_ino))
        for name in names:
            path = os.path.join(directory, name)
            if os.path.isfile(path):
                yield path


def drop(path):
    try:
        descriptor = os.open(path, os.O_RDONLY)
    except OSError:
        return 0
    try:
        os.posix_fadvise(descriptor, 0, 0, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(descriptor)
    return 1


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit("usage: evict.py RY [RY ...]")
    print(f"evicted {evict(sys.argv[1:])} files", file=sys.stderr)
