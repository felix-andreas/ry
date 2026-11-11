#!/usr/bin/env python3
"""Merges start image touch profiles into the order `ry-rcmd start-image --layout` lays the image out in.

A training run with `RY_IMAGE_TOUCH=FILE` writes one line per first touch of a 16-byte granule of
the loaded start image, in order: the address in hexadecimal and the phase (0 while the session
starts, 1 while a namespace loads after that, 2 otherwise). The image writer's `ids` file names
each object of that image by its stable id: pairs of little-endian 64-bit words, address then
size, the pair at index `id` for the object with that id.

Each object's place is its first touch across the runs: its lowest phase, and within that phase
the earliest position, measured as a fraction of its run's touches so that a long run does not
push every object it touches behind a short one's. The output lists the touched ids in that
order, one decimal id per line; the writer places the untouched objects after them, in id order.

usage: tools/layout/image_order.py IDS TOUCH...
"""
import bisect, pathlib, struct, sys


def objects(path):
    """The image's objects as parallel lists sorted by address: starts, ends and ids."""
    data = pathlib.Path(path).read_bytes()
    spans = sorted((address, address + size, id)
                   for id, (address, size) in enumerate(struct.iter_unpack("<QQ", data)))
    return [span[0] for span in spans], [span[1] for span in spans], [span[2] for span in spans]


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    starts, ends, ids = objects(sys.argv[1])
    places = {}
    for touch in sys.argv[2:]:
        lines = pathlib.Path(touch).read_text().split("\n")
        lines = [line for line in lines if line]
        for position, line in enumerate(lines):
            address, phase = line.split()
            address = int(address, 16)
            index = bisect.bisect_right(starts, address) - 1
            if index < 0 or address >= ends[index]:
                continue
            place = (int(phase), position / len(lines))
            id = ids[index]
            if place < places.get(id, (3, 0.0)):
                places[id] = place
    sys.stdout.write("".join(f"{id}\n" for id in sorted(places, key=places.get)))


if __name__ == "__main__":
    main()
