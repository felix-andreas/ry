"""Native-share census: how much of a run's time each JIT status of closure bodies takes.

Reads the file a `census` build writes for `RY_CENSUS=FILE` (crates/interp/src/census.rs) and
prints one row per status, with the number of bodies, their calls and their share of the sampled
time, then the 25 bodies that took the most time. Build and run:

    cargo build --release -p ry-lang -p libr -p rcmd --features jit/census
    RY_CENSUS=census.tsv ry run script.R
    python3 tools/native-share.py census.tsv [--exclude PREFIX]

`--exclude PREFIX` drops the bodies whose deparsed source starts with PREFIX from the total, such
as the function a parent session spends waiting on its callr children in. The rows of all code
objects compiled from one body (recompiles) and its AST runs fold into one body.
"""

import argparse
import collections
import csv


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("census")
    parser.add_argument("--exclude", default=None)
    arguments = parser.parse_args()
    with open(arguments.census) as file:
        rows = list(csv.DictReader(file, delimiter="\t", quoting=csv.QUOTE_NONE))
    bodies = collections.defaultdict(lambda: {"code": [], "tree": [], "name": ""})
    outside = 0
    for row in rows:
        if arguments.exclude and row["name"].startswith(arguments.exclude):
            continue
        if row["kind"] in ("top", "unknown"):
            outside += samples(row)
            continue
        body = bodies[row["source"]]
        body[row["kind"]].append(row)
        body["name"] = row["name"]
    by_status = collections.defaultdict(lambda: [0, 0, 0, 0])
    total = outside
    for body in bodies.values():
        rows_of_body = body["code"] + body["tree"]
        entry = by_status[status(body)]
        entry[0] += 1
        entry[1] += sum(int(row["calls"]) for row in rows_of_body)
        entry[2] += sum(samples(row) for row in rows_of_body)
        entry[3] += sum(int(row["samples_native"]) for row in rows_of_body)
        total += sum(samples(row) for row in rows_of_body)
    total = max(total, 1)
    print(f"{len(bodies)} closure bodies, {total} samples, {percent(outside, total)} outside any body")
    print()
    print("| status | bodies | calls | time | of which native |")
    print("|---|---|---|---|---|")
    for name, (count, calls, time, native) in sorted(by_status.items(), key=lambda item: -item[1][2]):
        print(f"| {name} | {count} | {calls} | {percent(time, total)} | {percent(native, total)} |")
    print()
    weight = lambda body: sum(samples(row) for row in body["code"] + body["tree"])
    for body in sorted(bodies.values(), key=weight, reverse=True)[:25]:
        print(f"{percent(weight(body), total):>6}  {status(body)[:40]:40}  {body['name'][:90]}")


def status(body):
    codes = body["code"]
    if codes:
        if any(int(code["native_entries"]) > 0 for code in codes):
            return "native"
        reasons = [reason for code in codes for reason in code["refusals"].split("|") if reason]
        if reasons:
            return reasons[-1]
        return "never hot"
    calls = sum(int(tree["calls"]) for tree in body["tree"])
    return "AST, called once" if calls <= 1 else "AST"


def samples(row):
    return int(row["samples_interp"]) + int(row["samples_native"])


def percent(part, whole):
    return f"{100 * part / whole:.1f}%"


main()
