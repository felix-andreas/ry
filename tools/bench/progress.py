#!/usr/bin/env python3
"""Measures ry releases for the progress dashboard and prints the rows to append to its datasets.

Every timing suite measures GNU R (`Rscript` on PATH) in the same sitting as the ry binaries it is
given, and every row carries the sitting, so the dashboard divides each ry time by the GNU R time
of its own sitting and never by a number from another machine or day. The output is one JSON
object that maps a dataset id to its new rows; `merge` appends such an object to the dataset files
downloaded from the dashboard. docs/src/content/docs/development/releasing.md has the procedure.

Usage:
  tools/bench/progress.py SUITE --release NAME=BINARY [--release ...] [options] > rows.json
  tools/bench/progress.py code --rev REV [--label LABEL] > rows.json
  tools/bench/progress.py merge rows.json DATASET_DIR

SUITE is one of programs, startup, memory, s4 and check. NAME is a release tag, or `main` for a
build of the checked-out commit, which is recorded as `main@<commit>`.
"""
import argparse, json, os, pathlib, platform, re, shutil, subprocess, sys, tempfile, time
from collections import Counter
from datetime import datetime, timezone

here = pathlib.Path(__file__).resolve().parent
root = here.parent.parent
why = here / "why"
repository = "https://github.com/felix-andreas/ry"
# Release archives built by nix name their interpreter by a /nix/store path, which most machines
# lack; the system loader runs them the same way.
loader = "/lib64/ld-linux-x86-64.so.2"
base_env = dict(os.environ, LC_ALL="C.UTF-8", R_DEFAULT_PACKAGES="stats")
datasets = {
    "programs": "perf_programs",
    "startup": "perf_startup",
    "memory": "perf_memory",
    "s4": "perf_s4",
    "check": "perf_check",
}

# The cases of `runtime_peak_memory_stays_near_gnu_r` in crates/ry/tests/test_perf.rs. Each
# prints its own peak resident set (VmHWM) in KiB, so the figure excludes the launcher.
memory_cases = {
    "rep": "id <- rep(1:1000, 50000)\ninvisible(gc())\n",
    "subset": "v <- runif(5e7)\nid <- rep(1:1000, 50000)\ninvisible(gc())\n"
              "for (i in 1:8) {\n  keep <- v > 0.5\n  s <- list(id[keep], v[keep])\n  rm(s, keep)\n}\n",
    "datatable": "if (!requireNamespace(\"data.table\", quietly = TRUE)) quit(status = 3)\nlibrary(data.table)\n"
                 "dt <- data.table(id = rep(1:1000, 5000), date = rep(seq_len(5000), each = 1000), v = runif(5e6))\n"
                 "invisible(gc())\nfor (i in 1:20) r <- dt[v > 0.5]\n",
}
print_peak = "line <- grep(\"^VmHWM\", readLines(\"/proc/self/status\"), value = TRUE)\ncat(gsub(\"\\\\D\", \"\", line), \"\\n\")\n"


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "merge":
        parser = argparse.ArgumentParser(prog="progress.py merge")
        parser.add_argument("rows")
        parser.add_argument("directory")
        args = parser.parse_args(sys.argv[2:])
        merge(pathlib.Path(args.rows), pathlib.Path(args.directory))
        return
    if len(sys.argv) > 1 and sys.argv[1] == "code":
        parser = argparse.ArgumentParser(prog="progress.py code")
        parser.add_argument("--rev", default="HEAD")
        parser.add_argument("--label", help="the sample's name on the chart (default: the tag, or main@<commit>)")
        args = parser.parse_args(sys.argv[2:])
        json.dump(code(args.rev, args.label), sys.stdout, indent=1)
        print()
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("suite", choices=sorted(datasets))
    parser.add_argument("--release", action="append", required=True, metavar="NAME=BINARY")
    parser.add_argument("--repeat", type=int, help="runs per engine; the fastest time and the largest peak are kept")
    parser.add_argument("--project", help="check: the package to check, such as finbench from tools/bench/generate.py")
    args = parser.parse_args()
    releases = [release(spec) for spec in args.release]
    sitting = {"measured": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "machine": machine(), "r": r_version()}
    suite = {"programs": suite_programs, "startup": suite_startup, "memory": suite_memory, "s4": suite_s4, "check": suite_check}[args.suite]
    options = {key: value for key, value in [("repeat", args.repeat), ("project", args.project)] if value is not None}
    rows = [{**sitting, **row, "origin": "measured"} for row in suite(releases, **options)]
    json.dump({datasets[args.suite]: rows}, sys.stdout, indent=1)
    print()


def release(spec):
    name, _, path = spec.partition("=")
    if not path:
        sys.exit(f"error: --release takes NAME=BINARY, got {spec!r}")
    rev = "HEAD" if name == "main" else f"{name}^{{commit}}"
    commit = git("rev-parse", "--short=10", rev).strip()
    date = git("log", "-1", "--format=%cs", rev).strip()
    command = [str(pathlib.Path(path).resolve())]
    # Reading only the head keeps this process small: a child's peak resident set as wait4
    # reports it never falls below its parent's at the time of the fork.
    with open(path, "rb") as binary:
        if b"/nix/store" in binary.read(4096):
            command = [loader] + command
    return {"release": f"main@{commit}" if name == "main" else name, "commit": commit, "commit_date": date, "command": command}


def suite_programs(releases, repeat=3):
    programs = sorted(path.stem for path in (why / "untyped").glob("*.R") if path.name != "harness.R")
    programs_commit = git("rev-parse", "--short=10", "HEAD").strip()
    engines = [(gnu_r_release(), "gnu_r", ["Rscript", "--vanilla"], "untyped")]
    for entry in releases:
        engines.append((entry, "ry", entry["command"] + ["run", "--runtime", "ry"], "untyped"))
        engines.append((entry, "ry_typed", entry["command"] + ["run", "--runtime", "ry"], "typed"))
    rows = []
    for program in programs:
        results = [[] for _ in engines]
        for _ in range(repeat):
            # Engines alternate, so no engine gets a quieter stretch of the machine than another.
            for index, (_, _, command, variant) in enumerate(engines):
                results[index].append(run_program(command, variant, program))
        for (entry, engine, _, _), runs in zip(engines, results):
            passed = [run for run in runs if "warm_s" in run]
            row = {**identity(entry), "program": program, "engine": engine, "repeat": repeat,
                   "source": f"tools/bench/progress.py programs; programs from tools/bench/why at {programs_commit}"}
            if passed:
                row.update(warm_s=min(run["warm_s"] for run in passed), cold_s=min(run["cold_s"] for run in passed),
                           peak_mib=round(max(run["peak_mib"] for run in passed), 1), checksum=passed[0]["checksum"])
            else:
                row.update(error=runs[-1]["error"])
            rows.append(row)
            print(json.dumps(row), file=sys.stderr, flush=True)
    return rows


def run_program(command, variant, program):
    # The scratch file sits beside the variant's ry.toml, so the typed run is checked in strict
    # mode and `ry run` refuses to run it if strict mode finds an error.
    scratch = why / variant / ".scratch-progress.R"
    scratch.write_text((why / variant / "harness.R").read_text() + "\n" + (why / variant / f"{program}.R").read_text())
    try:
        wall, peak, code, out, err = timed(command + [str(scratch)])
    finally:
        scratch.unlink()
    lines = [line for line in out.splitlines() if line.strip()]
    if code != 0 or not lines:
        return {"error": last_line(err, out, code)}
    _, checksum, warm, cold = lines[-1].split()
    return {"warm_s": float(warm), "cold_s": float(cold), "peak_mib": peak, "checksum": checksum}


def suite_startup(releases, repeat=10):
    with tempfile.TemporaryDirectory() as scratch:
        script = pathlib.Path(scratch) / "startup.R"
        script.write_text("cat(1L, \"\\n\")\n")
        engines = [(gnu_r_release(), "gnu_r", ["Rscript", "--vanilla"])]
        engines += [(entry, "ry", entry["command"] + ["run", "--runtime", "ry"]) for entry in releases]
        rows = []
        for entry, engine, command in engines:
            # The first run unpacks ry's embedded runtime, which happens once per installation.
            timed(command + [str(script)])
            runs = [timed(command + [str(script)]) for _ in range(repeat)]
            passed = [run for run in runs if run[2] == 0]
            row = {**identity(entry), "case": "hello", "engine": engine, "repeat": repeat,
                   "source": "tools/bench/progress.py startup: a script that prints one number, best of the runs after the first"}
            if passed:
                row.update(wall_s=round(min(run[0] for run in passed), 4), peak_mib=round(max(run[1] for run in passed), 1))
            else:
                row.update(error=last_line(runs[-1][4], runs[-1][3], runs[-1][2]))
            rows.append(row)
    return rows


def suite_memory(releases, repeat=2):
    engines = [(gnu_r_release(), "gnu_r", ["Rscript"])] + [(entry, "ry", entry["command"] + ["run"]) for entry in releases]
    rows = []
    with tempfile.TemporaryDirectory() as scratch:
        for case, body in memory_cases.items():
            script = pathlib.Path(scratch) / f"memory-{case}.R"
            script.write_text(body + print_peak)
            for entry, engine, command in engines:
                peaks, walls, error = [], [], None
                for _ in range(repeat):
                    wall, _, code, out, err = timed(command + [str(script)])
                    lines = [line for line in out.splitlines() if line.strip()]
                    if code == 0 and lines:
                        peaks.append(int(lines[-1]) / 1024)
                        walls.append(wall)
                    else:
                        error = last_line(err, out, code)
                row = {**identity(entry), "case": case, "engine": engine, "repeat": repeat,
                       "source": "tools/bench/progress.py memory: the cases of runtime_peak_memory_stays_near_gnu_r in crates/ry/tests/test_perf.rs, VmHWM"}
                if peaks:
                    row.update(peak_mib=round(max(peaks), 1), wall_s=round(min(walls), 3))
                else:
                    row.update(error=error)
                rows.append(row)
    return rows


def suite_s4(releases, repeat=3):
    rows = []
    with tempfile.TemporaryDirectory() as scratch:
        package = pathlib.Path(scratch) / "s4pkg"
        subprocess.run([sys.executable, "-I", str(root / "tools/loader-bench/s4_package.py"), str(package),
                        "--imports", "lubridate,data.table,xts"], check=True, stdout=subprocess.DEVNULL)
        script = pathlib.Path(scratch) / "s4-load.R"
        # Cls000 is virtual, so the first concrete class stands in for the first object a session makes.
        script.write_text(f"load_all({str(package)!r}, quiet = TRUE)\nx <- new(\"Cls020\")\ncat(class(x), \"\\n\")\n")
        for entry in releases:
            command = entry["command"] + ["run", "--runtime", "ry", str(script)]
            cache = pathlib.Path(scratch) / f"cache-{entry['commit']}"
            env = dict(base_env, XDG_CACHE_HOME=str(cache))
            # The first run unpacks the runtime into this cache; every later run reuses it.
            timed(command, env=env)
            colds, warms, error = [], [], None
            for _ in range(repeat):
                # A cold load has no S4 table and no load index, which are what a first load builds.
                for entry_path in (cache / "ry").glob("*"):
                    if entry_path.is_dir() and not entry_path.name.startswith(("runtime", "base")):
                        shutil.rmtree(entry_path)
                shutil.rmtree(package / ".ry_cache", ignore_errors=True)
                wall, _, code, out, err = timed(command, env=env)
                if code != 0:
                    error = last_line(err, out, code)
                    continue
                colds.append(wall)
                wall, _, code, out, err = timed(command, env=env)
                if code == 0:
                    warms.append(wall)
            row = {**identity(entry), "case": "s4_package", "repeat": repeat,
                   "source": "tools/bench/progress.py s4: tools/loader-bench/s4_package.py --imports lubridate,data.table,xts, then `ry run` of load_all() and one new(), whole process"}
            if colds:
                row.update(cold_s=round(min(colds), 3), warm_s=round(min(warms), 3) if warms else None)
            else:
                row.update(error=error)
            rows.append(row)
    return rows


def suite_check(releases, repeat=3, project=None):
    if project is None:
        sys.exit("error: check needs --project, such as the finbench package from "
                 "`tools/bench/generate.py --domain finance --lines 300000 --files 500`")
    project = pathlib.Path(project).resolve()
    lines = sum(path.read_text().count("\n") for path in (project / "R").glob("*.R"))
    rows = []
    for entry in releases:
        walls, peaks, codes = [], [], set()
        for _ in range(repeat):
            # Releases before 0.3.1 named the tool roughly, and its cache with it.
            for cache in (".ry_cache", ".roughly_cache"):
                shutil.rmtree(project / cache, ignore_errors=True)
            wall, peak, code, _, _ = timed(entry["command"] + ["check", "."], cwd=str(project))
            walls.append(wall)
            peaks.append(peak)
            codes.add(code)
        for cache in (".ry_cache", ".roughly_cache"):
            shutil.rmtree(project / cache, ignore_errors=True)
        rows.append({**identity(entry), "case": f"{project.name}_cold_check", "lines": lines, "repeat": repeat,
                     "wall_s": round(min(walls), 3), "peak_mib": round(max(peaks), 1), "exit_codes": sorted(codes),
                     "source": f"tools/bench/progress.py check: `check .` in {project.name}, no cache"})
    return rows


def code(rev, label):
    """Lines of Rust per crate and the test asset counts at one commit."""
    commit = git("rev-parse", f"{rev}^{{commit}}").strip()
    committed = datetime.fromisoformat(git("log", "-1", "--format=%cI", commit).strip())
    at = committed.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    tags = git("tag", "--points-at", commit).split()
    label = label or (tags[0] if tags else f"main@{commit[:10]}")
    listing = git("ls-tree", "-r", "--name-only", commit).splitlines()
    sources = [path for path in listing if path.endswith(".rs")]
    lines = Counter()
    if sources:
        batch = subprocess.run(["git", "-C", str(root), "cat-file", "--batch"], capture_output=True, check=True,
                               input="".join(f"{commit}:{path}\n" for path in sources).encode()).stdout
        position = 0
        for path in sources:
            header_end = batch.index(b"\n", position)
            size = int(batch[position:header_end].split()[2])
            body = batch[header_end + 1:header_end + 1 + size]
            position = header_end + 1 + size + 1
            parts = path.split("/")
            lines[parts[1] if parts[0] == "crates" and len(parts) > 2 else parts[0]] += body.count(b"\n")
    source = f"{repository}/tree/{commit[:10]}"
    sample = {"label": label, "commit": commit[:10], "at": at, "origin": "measured", "source": source}
    return {
        "codebase_loc": [{**sample, "crate": crate, "lines": count} for crate, count in sorted(lines.items())],
        "codebase_tests": [{
            **sample,
            "rust_lines": sum(lines.values()),
            "fixture_files": sum(1 for path in listing if path.endswith(".test")),
            "oracle_cases": sum(1 for path in listing if path.startswith("crates/oracle/cases/") and path.endswith(".R")),
            "scenarios": sum(1 for path in listing if path.endswith(".scenario")),
        }],
    }


def merge(rows_path, directory):
    """Appends new rows to the dataset files in `directory`, skipping a row that is already there."""
    new = json.loads(rows_path.read_text())
    for dataset, rows in new.items():
        path = directory / f"{dataset}.json"
        existing = json.loads(path.read_text()) if path.exists() else []
        seen = {json.dumps(row, sort_keys=True) for row in existing}
        added = [row for row in rows if json.dumps(row, sort_keys=True) not in seen]
        path.write_text(json.dumps(existing + added, indent=1) + "\n")
        print(f"{path}: {len(added)} rows added, {len(existing) + len(added)} in all")


def gnu_r_release():
    return {"release": "GNU R", "commit": "", "commit_date": ""}


def identity(entry):
    return {key: entry[key] for key in ("release", "commit", "commit_date")}


def timed(command, env=None, cwd=None):
    """Wall seconds, peak resident MiB, exit code, stdout and stderr of one run."""
    with tempfile.TemporaryFile("w+") as out, tempfile.TemporaryFile("w+") as err:
        started = time.perf_counter()
        child = subprocess.Popen(command, stdout=out, stderr=err, text=True, env=env or base_env, cwd=cwd)
        # wait4 reports the child's own peak resident set in KiB on Linux.
        _, status, usage = os.wait4(child.pid, 0)
        wall = time.perf_counter() - started
        out.seek(0)
        err.seek(0)
        return wall, usage.ru_maxrss / 1024, os.waitstatus_to_exitcode(status), out.read(), err.read()


def last_line(err, out, code):
    return ((err.strip() or out.strip()).splitlines() or [f"exit status {code}"])[-1][:160]


def machine():
    cpu = platform.machine()
    for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
        if line.startswith("model name"):
            cpu = re.sub(r"\s+", " ", line.split(":", 1)[1].strip())
            break
    return f"{cpu}, {os.cpu_count()} cores, {platform.system()} {platform.release()}"


def r_version():
    result = subprocess.run(["Rscript", "--version"], capture_output=True, text=True)
    return (result.stdout + result.stderr).strip().splitlines()[0]


def git(*args):
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True, check=True).stdout


main()
