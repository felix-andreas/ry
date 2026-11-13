#!/usr/bin/env python3
"""Tests of bench.py: its release-archive mode (what results.json records about the archive, and
which images the run accepts) and the typed suite, which scores typed ry against Rust.

A fake archive stands in for a release: its `ry` is a shell script that, like the real one,
extracts a runtime library (a C library built here that exports `ry_runtime_id`) and a base R tree
into the cache directory on its first start, writes an R home whose `.libr` names that library, and
prints `R.home()`. The typed suite's tests feed `summarize` rounds written by hand, and compile one
small Rust program against the suite's own harness.rs. crates/ry/tests/test_release_metadata.rs runs
this file.

usage: python3 -I tools/bench/test_bench.py
"""
import argparse, hashlib, importlib.util, io, os, pathlib, shutil, subprocess, sys, tarfile, tempfile, unittest
from contextlib import redirect_stdout

here = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("bench", here / "bench.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)

IDENTITY = "release0123456789"
FAKE_RY = """#!/bin/sh
case "$1" in
--version) echo "ry {version}" ;;
run)
  runtime="$XDG_CACHE_HOME/ry/runtime/k" base="$XDG_CACHE_HOME/ry/base/k" home="$XDG_CACHE_HOME/ry/home/k"
  if [ ! -d "$home" ]; then
    mkdir -p "$runtime" "$base/library" "$home/library"
    cp "$(dirname "$0")/payload/libR.so" "$runtime/libR.so"
    cp -R "$(dirname "$0")/payload/library/." "$base/library/"
    for package in "$base/library/"*; do ln -s "$package" "$home/library/"; done
    printf '%s\\n%s\\n%s' "$runtime/libR.so" "$0" "$base" > "$home/.libr"
  fi
  printf '%s' "$home" ;;
esac
"""


class ReleaseArchive(unittest.TestCase):
    def setUp(self):
        self.directory = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-test-"))
        self.addCleanup(shutil.rmtree, self.directory, ignore_errors=True)
        saved = dict(os.environ)
        self.addCleanup(lambda: (os.environ.clear(), os.environ.update(saved)))
        for variable in ("R_LIBS", "R_LIBS_USER"):
            os.environ.pop(variable, None)
        corpus = self.directory / "corpus"
        (corpus / "library").mkdir(parents=True)
        original = bench.corpus.corpus
        bench.corpus.corpus = corpus
        self.addCleanup(setattr, bench.corpus, "corpus", original)
        self.corpus_library = corpus / "library"

    def archive(self, version="0.4.4-beta"):
        stage = self.directory / "stage"
        payload = stage / "payload"
        (payload / "library/base/R").mkdir(parents=True)
        (payload / "library/stats/R").mkdir(parents=True)
        source = self.directory / "id.c"
        # bench.py refuses a runtime that cannot report an unused image, which it detects by the
        # name of the variable that turns the report on.
        source.write_text(f'const char *ry_runtime_id(void) {{ return "{IDENTITY}"; }}\n'
                          'const char *ry_image_log(void) { return "RY_IMAGE_LOG"; }\n')
        subprocess.run(["cc", "-shared", "-fPIC", "-o", payload / "libR.so", source], check=True)
        (payload / f"library/base/R/ry-start.{IDENTITY}.image").write_bytes(b"RYSTART\0")
        (payload / f"library/stats/R/stats.{IDENTITY}.ryc").write_bytes(b"RYCODE\0\3")
        ry = stage / "ry"
        ry.write_text(FAKE_RY.format(version=version))
        ry.chmod(0o755)
        archive = self.directory / "ry-x86_64-unknown-linux-gnu.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            for path in sorted(stage.rglob("*")):
                tar.add(path, arcname=str(path.relative_to(stage)), recursive=False)
        return archive

    def test_records_the_tag_and_the_archives_sha256(self):
        archive = self.archive()
        ry, release = bench.unpack_release(archive)
        self.assertEqual(release, {"tag": "0.4.4-beta", "nightly": False, "archive": archive.name,
                                   "sha256": hashlib.sha256(archive.read_bytes()).hexdigest()})
        self.assertTrue(ry.endswith("/archive/ry"))
        self.assertNotIn("R_HOME", os.environ)

    def test_names_a_nightly_by_its_build_name(self):
        _, release = bench.unpack_release(self.archive("0.4.3-beta (nightly.20261110.1955.a15ce0)"))
        self.assertEqual((release["tag"], release["nightly"]), ("nightly.20261110.1955.a15ce0", True))
        self.assertEqual(bench.release_tag("ry 0.4.3-beta (iterate build)"), (None, False))

    def test_accepts_the_archives_own_images(self):
        ry, _ = bench.unpack_release(self.archive())
        bench.refuse_foreign_images([ry])

    def test_refuses_a_library_image_another_build_wrote(self):
        ry, _ = bench.unpack_release(self.archive())
        foreign = self.corpus_library / "zoo/R/zoo.otherbuild.ryc"
        foreign.parent.mkdir(parents=True)
        foreign.write_bytes(b"RYCODE\0\3")
        with self.assertRaises(SystemExit) as refusal, redirect_stdout(io.StringIO()):
            bench.refuse_foreign_images([ry])
        self.assertIn(f"runtime {IDENTITY}", str(refusal.exception))
        self.assertTrue(str(refusal.exception).endswith("reinstall them with this build: zoo"))


class NoRefusals:
    """Stands in for a runner where `summarize` asks which typed programs fell back."""

    def typed_refusal(self, program, engine):
        return None

    def compile_seconds(self, program):
        return {"rust": 0.25, "ry_typed": 0.002}


class TypedSuite(unittest.TestCase):
    def setUp(self):
        self.args = argparse.Namespace(ry="ry", baseline=None, jit_off=False, rscript="/no/such/Rscript",
                                       seed=1, rounds=3, extra_rounds=0)
        self.table = bench.engines(self.args, bench.RUST_SUITE)

    def summarize(self, rounds):
        """The results of `rounds`, each a map from a program to its `(seconds, checksum)` on Rust
        and on typed ry."""
        programs = [{"id": name, "display": name, "typed_source": f"{name}.typed.R"} for name in rounds[0]]
        state = {"rounds": [{name: {engine.key: {"seconds": seconds, "checksum": checksum, "peak_mib": 1.0}
                                    for engine, (seconds, checksum) in zip(self.table, pair)}
                             for name, pair in round_.items()} for round_ in rounds]}
        return bench.summarize(bench.RUST_SUITE, {"name": "Typed ry against Rust"}, programs, self.table, state,
                               self.args, {}, NoRefusals())

    def test_lists_every_typed_program_beside_its_rust_twin(self):
        typed = {path.name.removesuffix(".typed.R") for owner in ("kernels", "applications")
                 for path in (here / owner).glob("*.typed.R")}
        programs = bench.load_suite(bench.RUST_SUITE)["programs"]
        self.assertEqual({program["id"] for program in programs}, typed)
        for program in programs:
            self.assertTrue((bench.root / program["rust_source"]).is_file(), program["rust_source"])

    def test_runs_rust_and_typed_ry_without_gnu_r(self):
        self.assertEqual([(engine.key, engine.typed) for engine in self.table], [("rust", False), ("ry_typed", True)])
        self.assertEqual(bench.baseline_key(bench.RUST_SUITE), self.table[0].key)

    def test_a_ratio_is_typed_rys_time_over_rusts(self):
        results = self.summarize([{"double": ((0.010, "7"), (0.020, "7")), "half": ((0.040, "3"), (0.020, "3"))},
                                  {"double": ((0.012, "7"), (0.024, "7")), "half": ((0.040, "3"), (0.020, "3"))},
                                  {"double": ((0.011, "7"), (0.022, "7")), "half": ((0.044, "3"), (0.022, "3"))}])
        self.assertEqual((results["baseline"], results["ratio_kind"]), ("rust", "slowdown"))
        ratios = {row["id"]: row["ratio"]["ry_typed"] for row in results["programs"]}
        self.assertEqual(ratios["double"], {"median": 2.0, "low": 2.0, "high": 2.0})
        self.assertEqual(ratios["half"], {"median": 0.5, "low": 0.5, "high": 0.5})
        self.assertEqual(results["geomean"]["ry_typed"], {"median": 1.0, "low": 1.0, "high": 1.0, "programs": 2})
        self.assertEqual(results["programs"][0]["seconds"], {"rust": 0.011, "ry_typed": 0.022})
        self.assertEqual(bench.ratio_label(results, "ry_typed"), "ry typed / Rust")
        # `VmHWM` starts afresh at `exec`, so a Rust program's peak is its own and is recorded.
        self.assertEqual(results["programs"][0]["peak_mib"], {"rust": 1.0, "ry_typed": 1.0})
        self.assertEqual(results["programs"][0]["compile_seconds"], {"rust": 0.25, "ry_typed": 0.002})

    def test_a_program_rust_finishes_too_fast_to_time_stays_out_of_the_mean(self):
        floor = bench.RUST_SHORT
        results = self.summarize([{"timed": ((0.010, "7"), (0.030, "7")),
                                   "short": ((floor / 2, "3"), (floor * 40, "3"))}] * 3)
        self.assertEqual(results["meta"]["too_short_seconds"], floor)
        short = results["programs"][1]
        self.assertEqual(short["flags"], [{"kind": "too_short", "engine": "rust"}])
        self.assertEqual(short["ratio"]["ry_typed"]["median"], 80.0)
        self.assertEqual(results["geomean"]["ry_typed"], {"median": 3.0, "low": 3.0, "high": 3.0, "programs": 1})

    def test_a_checksum_that_differs_from_rusts_fails_the_program(self):
        results = self.summarize([{"wrong": ((0.010, "7"), (0.020, "8")),
                                   "close": ((0.010, "0.30000000000000004"), (0.020, "0.3"))}] * 3)
        wrong, close = results["programs"]
        self.assertEqual(wrong["failures"], [{"engine": "ry_typed", "reason": "checksum 8, Rust printed 7"}])
        self.assertEqual(wrong["ratio"], {})
        self.assertNotIn("compile_seconds", wrong)
        self.assertNotIn("failures", close)
        self.assertEqual(results["geomean"]["ry_typed"]["programs"], 1)

    def test_reads_the_compile_time_off_a_typed_log_line(self):
        compiled = bench.COMPILED.search("typed: compiled for [List(Str), List(F64)] -> F64 in 2693 us")
        self.assertEqual(compiled["microseconds"], "2693")
        self.assertIsNone(bench.COMPILED.search("typed: `pipeline` runs in the interpreter: it calls `paste`"))

    def test_a_rust_program_reports_what_the_typed_harness_reports(self):
        directory = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-test-"))
        self.addCleanup(shutil.rmtree, directory, ignore_errors=True)
        shutil.copy(here / bench.RUST_SUITE / "harness.rs", directory / "harness.rs")
        source = directory / "answer.rs"
        source.write_text("mod harness;\n\nfn main() {\n"
                          "    harness::measure(\"answer\", || std::hint::black_box(40.5) + 1.5);\n}\n")
        runner = bench.RustRunner(bench.RUST_SUITE, self.args)
        self.addCleanup(runner.close)
        program = {"id": "answer", "rust_source": str(source)}
        rust = bench.Engine("rust", "Rust", None, dict(os.environ, RY_BENCH_REPEAT="4"))
        result = runner.run(program, rust)
        self.assertEqual(result["checksum"], "42")
        self.assertGreater(runner.compile_seconds(program)["rust"], 0.0)
        self.assertNotIn("ry_typed", runner.compile_seconds(program))
        self.assertGreaterEqual(result["cold"], 0.0)
        self.assertGreaterEqual(result["seconds"], 0.0)
        refused = runner.run(program, bench.Engine("rust", "Rust", None, dict(os.environ, RY_BENCH_REPEAT="none")))
        self.assertEqual(refused, {"seconds": None, "reason": "RY_BENCH_REPEAT must be a positive whole number, not `none`"})


class PublicSuite(unittest.TestCase):
    def test_every_program_has_a_typed_source_that_prints_its_own_name(self):
        for program in bench.load_suite("public")["programs"]:
            typed = bench.root / program["typed_source"]
            name = typed.name.removesuffix(".typed.R")
            self.assertIn(f'sprintf("{name} %s', typed.read_text(), program["id"])

    def test_a_shared_typed_source_is_read_by_the_name_it_prints(self):
        directory = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-test-"))
        self.addCleanup(shutil.rmtree, directory, ignore_errors=True)
        for name in ("loop.R", "loop_2.R", "loop.typed.R"):
            (directory / name).write_text("")
        program = {"id": "loop_2", "source": str(directory / "loop_2.R"),
                   "typed_source": str(directory / "loop.typed.R")}
        # The engine prints a line for each name, so only the right one gives these numbers.
        engine = argparse.Namespace(typed=True, env=dict(os.environ), command=[
            sys.executable, "-c", "print('loop 1:2 0.5'); print('loop_2 3:4 9.0')"])
        runner = bench.PublicRunner("public", None)
        self.addCleanup(runner.close)
        outcome = runner.run(program, engine)
        self.assertEqual((outcome["seconds"], outcome["checksum"]), (0.5, "1:2"))


class BuildProfile(unittest.TestCase):
    def test_names_a_build_that_is_not_a_release(self):
        directory = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-test-"))
        self.addCleanup(shutil.rmtree, directory, ignore_errors=True)
        ry = directory / "ry"
        for version, expected in (("0.4.4-beta", None), ("0.4.3-beta (iterate build)", "ry 0.4.3-beta (iterate build)"),
                                  ("0.4.3-beta (nightly.20261110.1955.a15ce0)", None)):
            ry.write_text(FAKE_RY.format(version=version))
            ry.chmod(0o755)
            self.assertEqual(bench.unreleased_build(str(ry)), expected)


if __name__ == "__main__":
    unittest.main()
