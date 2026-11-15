#!/usr/bin/env python3
"""Checks, and restores, the PT_GNU_STACK program header of ELF shared objects and executables.

Without PT_GNU_STACK, glibc assumes the object needs an executable stack, and since glibc 2.41
`dlopen` refuses such a library. BOLT's `-use-gnu-stack` turns the PT_GNU_STACK entry into the
PT_LOAD of the code it moves, because the program header table has no room for another entry.
`restore` writes the header back over the PT_NOTE entry (the build-id note stays in the section
table), and `check` fails for a file whose PT_GNU_STACK is missing or executable.

usage: tools/layout/gnu_stack.py check FILE...
       tools/layout/gnu_stack.py restore FILE
"""
import struct, sys

PT_NOTE, PT_GNU_STACK, PF_X, PF_RW = 4, 0x6474E551, 1, 6


def entries(data):
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise ValueError("not a little-endian 64-bit ELF file")
    offset, = struct.unpack_from("<Q", data, 0x20)
    size, count = struct.unpack_from("<HH", data, 0x36)
    return [(offset + index * size, struct.unpack_from("<II", data, offset + index * size)) for index in range(count)]


def stack_flags(data):
    for _, (kind, flags) in entries(data):
        if kind == PT_GNU_STACK:
            return flags
    return None


def check(paths):
    failed = False
    for path in paths:
        with open(path, "rb") as file:
            flags = stack_flags(file.read(1 << 20))
        problem = "has no PT_GNU_STACK header" if flags is None else "has an executable PT_GNU_STACK" if flags & PF_X else None
        if problem:
            print(f"error: {path} {problem}, so glibc 2.41 and later refuse to load it", file=sys.stderr)
            failed = True
    return failed


def restore(path):
    with open(path, "r+b") as file:
        data = file.read(1 << 20)
        if stack_flags(data) is not None:
            return
        note = next((position for position, (kind, _) in entries(data) if kind == PT_NOTE), None)
        if note is None:
            sys.exit(f"error: {path} has neither PT_GNU_STACK nor a PT_NOTE entry to reuse")
        file.seek(note)
        file.write(struct.pack("<IIQQQQQQ", PT_GNU_STACK, PF_RW, 0, 0, 0, 0, 0, 16))


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "check":
        sys.exit(1 if check(sys.argv[2:]) else 0)
    if len(sys.argv) == 3 and sys.argv[1] == "restore":
        restore(sys.argv[2])
        return
    sys.exit(__doc__.strip().splitlines()[-2].strip() + "\n" + __doc__.strip().splitlines()[-1])


main()
