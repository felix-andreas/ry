#!/usr/bin/env python3
"""Prints the symbols of LIBRARY whose functions ran in the PGO training, one per line.

tools/layout/optimize.sh hands the list to BOLT, which then instruments only these functions.
A function the profile records but that never ran is left out, and so is one that LTO inlined
everywhere, since it has no symbol of its own. Local functions carry their codegen unit as a
`cgu;` prefix in the profile, and LTO may suffix a symbol with `.llvm.<hash>`; both are dropped
for the match, and a local symbol is printed with the `/1` suffix BOLT gives it.

usage: tools/layout/hot_functions.py LLVM_PROFDATA PROFILE LIBRARY
"""
import re, subprocess, sys


def main():
    profdata, profile, library = sys.argv[1:4]
    listing = subprocess.run([profdata, "show", "--all-functions", "--counts", profile],
                             capture_output=True, text=True, check=True).stdout
    ran = set()
    name = None
    for line in listing.splitlines():
        if line.startswith("  ") and not line.startswith("    ") and line.endswith(":"):
            name = line.strip()[:-1].split(";")[-1]
        elif line.startswith("    Block counts:") and name and re.search(r"[1-9]", line):
            ran.add(name)
    symbols = subprocess.run(["nm", "--defined-only", library], capture_output=True, text=True,
                             check=True).stdout
    chosen = set()
    for line in symbols.splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[1] in "tTwW" and re.sub(r"\.llvm\.\d+$", "", fields[2]) in ran:
            # BOLT names a local symbol `name/1`, and LTO makes nearly every Rust function local.
            chosen.add(fields[2] + "/1" if fields[1] == "t" else fields[2])
    if not chosen:
        sys.exit(f"hot_functions: no symbol of {library} has a count in {profile}")
    print(f"hot_functions: {len(chosen)} of {len(ran)} functions that ran have a symbol", file=sys.stderr)
    print("\n".join(sorted(chosen)))


if __name__ == "__main__":
    main()
