#!/usr/bin/env python3
"""Runs the training workload for profile-guided optimization and BOLT on an instrumented `ry`.

Every program runs in a fresh `ry run --runtime ry` process, once with the JIT at its default and
once with `RY_JIT=off`, so both tiers' paths are in the profile. The mix:

- every oracle case (crates/oracle/cases/*.R) through the oracle's driver, so a case that stops
  at an error still runs to its end, as it does under `oracle compare`;
- every plain R program of the numeric kernels and application workloads (tools/bench/kernels and
  tools/bench/applications) with its harness and
  `RY_BENCH_REPEAT=1`;
- the training half of the vendored public suites (tools/bench/public): the `rcp` and
  `shootout` programs at even positions in sorted order. The odd half and the whole `awfy` suite
  are held out, so a measurement on them shows whether the profile generalizes (`--held-out`
  prints those programs).

The instrumented binaries write their profiles wherever their build put them (LLVM_PROFILE_FILE
for PGO, the BOLT runtime's file for BOLT); this script only runs programs. A program that fails
is reported but does not stop the training, because a failure on the instrumented build is still
a valid profile of the path up to it. The exit status is non-zero when nothing ran or when a
workload of the mix failed, since the mix is the core of the profile. The workloads need
testthat, purrr, stringr, lubridate, data.table, xts and zoo installed.

With `--image-touch DIR`, each run also records which objects of the start image it touches, in
`DIR/<n>.touch` (`RY_IMAGE_TOUCH`), and an empty program joins the mix, so the profile holds a
start that runs nothing too. `tools/layout/image_order.py` merges the files into the order the
next image is laid out in.

usage: tools/layout/train.py RY [--jobs N] [--held-out] [--image-touch DIR]
"""
import argparse, concurrent.futures, os, pathlib, subprocess, sys, tempfile

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent
bench = root / "tools/bench"
# The instrumented build runs up to a few times slower than the release build.
TIMEOUT = 900
# Suites no training run sees, so they measure the profile on programs of a kind it never ran.
HELD_OUT_SUITES = {"awfy"}


def public_programs():
    """The public suite programs split into the training half and the held-out half."""
    programs = sorted((bench / "public").glob("*/*.R"))
    trained = [program for program in programs[0::2] if program.parent.name not in HELD_OUT_SUITES]
    return trained, [program for program in programs if program not in trained]


def jobs(ry, tier, scratch, image_touch):
    driver = root / "crates/oracle/src/driver.R"
    if image_touch:
        empty = scratch / "empty.R"
        empty.write_text("")
        yield "empty", [ry, "run", "--runtime", "ry", str(empty)], scratch
    for case in sorted((root / "crates/oracle/cases").glob("*.R")):
        report = scratch / f"{tier}-{case.stem}.json"
        yield f"oracle/{case.stem}", [ry, "run", "--runtime", "ry", str(driver), str(case), str(report)], case.parent
    harness = (bench / "harness.R").read_text()
    for workload in sorted([*bench.glob("kernels/*.R"), *bench.glob("applications/*.R")]):
        if workload.name.endswith(".typed.R"):
            continue
        script = scratch / f"{tier}-workload-{workload.name}"
        script.write_text(harness + "\n" + workload.read_text())
        yield f"workload/{workload.stem}", [ry, "run", "--runtime", "ry", str(script)], bench
    training, _ = public_programs()
    for program in training:
        yield f"public/{program.parent.name}/{program.stem}", [ry, "run", "--runtime", "ry", program.name], program.parent


def run(name, command, cwd, env):
    try:
        child = subprocess.run(command, cwd=cwd, env=env, capture_output=True, text=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        return name, f"timed out after {TIMEOUT} s"
    if child.returncode != 0:
        lines = (child.stderr.strip() or child.stdout.strip() or "failed").splitlines()
        return name, f"exit {child.returncode}: {lines[-1] if lines else ''}"
    return name, None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("ry")
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--held-out", action="store_true", help="print the held-out public programs and exit")
    parser.add_argument("--image-touch", type=pathlib.Path, help="record each run's start image touches in this directory")
    args = parser.parse_args()
    if args.held_out:
        for program in public_programs()[1]:
            print(program.relative_to(root))
        return
    base = dict(os.environ, LANG="C.UTF-8", LC_ALL="C.UTF-8", TZ="UTC", RY_BENCH_REPEAT="1")
    base.pop("RY_JIT", None)
    tiers = [("jit", base), ("nojit", dict(base, RY_JIT="off"))]
    with tempfile.TemporaryDirectory(prefix="ry-train-") as scratch:
        scratch = pathlib.Path(scratch)
        work = [(f"{tier}:{name}", command, cwd, env)
                for tier, env in tiers for name, command, cwd in jobs(args.ry, tier, scratch, args.image_touch)]
        if args.image_touch:
            args.image_touch.mkdir(parents=True, exist_ok=True)
            work = [(name, command, cwd, dict(env, RY_IMAGE_TOUCH=str(args.image_touch / f"{index}.touch")))
                    for index, (name, command, cwd, env) in enumerate(work)]
        failed = 0
        broken_workloads = []
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for name, error in pool.map(lambda job: run(*job), work):
                if error:
                    failed += 1
                    if "workload/" in name:
                        broken_workloads.append(name)
                    print(f"train: {name}: {error}", file=sys.stderr)
    print(f"train: ran {len(work)} programs, {failed} failed", file=sys.stderr)
    # A workload that cannot run, usually for a missing package, leaves its paths out of the
    # profile, and that profile then optimizes them as cold.
    if failed == len(work) or broken_workloads:
        sys.exit(f"train: the workload mix must run in full; failed: {', '.join(broken_workloads)}"
                 if broken_workloads else 1)


if __name__ == "__main__":
    main()
