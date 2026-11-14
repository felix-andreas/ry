#!/usr/bin/env python3
"""Tests of bench.py: its release-archive mode (what results.json records about the archive, and
which images the run accepts), the typed suite, which scores typed ry against Rust, and the hot
suite's runner and its generator, tooling/hot.py.

A fake archive stands in for a release: its `ry` is a shell script that, like the real one,
extracts a runtime library (a C library built here that exports `ry_runtime_id`) and a base R tree
into the cache directory on its first start, writes an R home whose `.libr` names that library, and
prints `R.home()`. The typed suite's tests feed `summarize` rounds written by hand, and compile one
small Rust program against the suite's own harness.rs. crates/ry/tests/test_release_metadata.rs runs
this file.

usage: python3 -I tools/bench/test_bench.py
"""
import argparse, hashlib, importlib.util, io, math, os, pathlib, shutil, subprocess, sys, tarfile, tempfile, unittest
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
        self.assertGreaterEqual(result["first"], 0.0)
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
        engine = argparse.Namespace(typed=True, ahead=False, env=dict(os.environ), command=[
            sys.executable, "-c", "print('loop 1:2 0.5'); print('loop_2 3:4 9.0')"])
        runner = bench.PublicRunner("public", None)
        self.addCleanup(runner.close)
        outcome = runner.run(program, engine)
        self.assertEqual((outcome["seconds"], outcome["checksum"]), (0.5, "1:2"))


spec = importlib.util.spec_from_file_location("hot", here / "tooling/hot.py")
hot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hot)


class HotSuite(unittest.TestCase):
    def setUp(self):
        self.args = argparse.Namespace(seed=1, rounds=3, extra_rounds=0)
        self.table = [bench.Engine("gnu_r", "GNU R", None, {}), bench.Engine("ry", "ry", None, {})]
        self.directory = pathlib.Path(tempfile.mkdtemp(prefix="ry-bench-test-"))
        self.addCleanup(shutil.rmtree, self.directory, ignore_errors=True)

    def program(self, name, weight, **fields):
        return dict({"id": name, "display": name, "samples": [f"samples/{name}-1.rds"], "iterations": [100],
                     "rng": [False], "threw": [False], "namespaces": [], "gnu_seconds_per_call": 1e-4,
                     "share": dict(weight), "weight": dict(weight)}, **fields)

    def summarize(self, programs, rounds):
        """The results of `rounds`, each a map from a program to its batch seconds on GNU R and ry."""
        state = {"rounds": [{name: {engine.key: {"seconds": seconds, "min": seconds, "checksum": "num:1:0:2:1.00000000000", "calls": 100}
                                    if seconds else {"seconds": None, "reason": "a call failed: no"}
                                    for engine, seconds in zip(self.table, pair)}
                             for name, pair in round_.items()} for round_ in rounds]}
        return bench.summarize(bench.HOT_SUITE, {"name": "Hot package entries"}, programs, self.table, state,
                               self.args, {}, None)

    def test_a_suite_that_was_never_generated_says_how_to_generate_it(self):
        with self.assertRaises(SystemExit) as refusal:
            bench.load_hot_suite(self.directory)
        self.assertIn("has not been generated", str(refusal.exception))
        self.assertIn("just bench-hot-capture", str(refusal.exception))

    def test_a_program_needs_iterations_and_flags_for_every_sample(self):
        (self.directory / "samples").mkdir()
        (self.directory / "samples/sd-1.rds").write_bytes(b"")
        program = self.program("sd", {"finbench": 1.0}, iterations=[1, 2])
        (self.directory / "suite.toml").write_text(
            hot.render_toml({"id": "hot", "session": ["stats"], "selection": {}, "programs": [program]}))
        with self.assertRaises(SystemExit) as refusal:
            bench.load_hot_suite(self.directory)
        self.assertIn("gives sd 1 samples but not as many iterations", str(refusal.exception))
        program["iterations"] = [1]
        (self.directory / "suite.toml").write_text(
            hot.render_toml({"id": "hot", "session": ["stats"], "selection": {}, "programs": [program]}))
        self.assertEqual(bench.load_hot_suite(self.directory)["programs"], [program])

    def test_reads_a_harness_line(self):
        self.assertEqual(bench.parse_hot_line("stats-sd num:1:0:1.5:1.00000000000,error 0.019000 0.020500 2044"),
                         ("stats-sd", {"checksum": "num:1:0:1.5:1.00000000000,error", "calls": 2044, "seconds": 0.0205, "min": 0.019}))
        self.assertEqual(bench.parse_hot_line("stats-sd num:1:0:1.5:1.00000000000 NA NA 2044"),
                         ("stats-sd", {"checksum": "num:1:0:1.5:1.00000000000", "calls": 2044}))
        self.assertEqual(bench.parse_hot_line("zoo-rollmean failed namespace zoo does not load: gone"),
                         ("zoo-rollmean", {"seconds": None, "reason": "namespace zoo does not load: gone"}))
        self.assertIsNone(bench.parse_hot_line("Loading required namespace: data.table"))

    def test_the_program_table_lists_each_samples_flags(self):
        program = self.program("sd", {}, samples=["samples/sd-1.rds", "samples/sd-2.rds"], iterations=[10, 20],
                               rng=[True, False], threw=[False, True], namespaces=["stats", "zoo"])
        self.assertEqual(bench.hot_program_table([program]),
                         "id\tsamples\titerations\trng\tthrew\tnamespaces\n"
                         "sd\tsamples/sd-1.rds,samples/sd-2.rds\t10,20\t1,0\t0,1\tstats,zoo\n")

    def test_a_program_the_harness_printed_no_line_for_fails(self):
        engine = bench.Engine("ry", "ry", [sys.executable, "-c", "print('sd num:1:0:2:1.00000000000 0.01 0.02 7')"], dict(os.environ))
        (self.directory / "suite.toml").write_text(hot.render_toml({"id": "hot", "session": ["stats"], "selection": {}, "programs": []}))
        runner = bench.HotRunner(bench.HOT_SUITE, self.args, directory=self.directory)
        self.addCleanup(runner.close)
        measured = runner.run_batch([self.program("sd", {}), self.program("median", {})], engine)
        self.assertEqual(measured["sd"], {"checksum": "num:1:0:2:1.00000000000", "calls": 7, "seconds": 0.02, "min": 0.01})
        self.assertEqual(measured["median"], {"seconds": None, "reason": "the harness printed no line for it"})

    def test_the_predicted_speedup_is_the_weighted_harmonic_mean(self):
        programs = [self.program("fast", {"finbench": 0.75, "dsbench": 0.5}),
                    self.program("slow", {"finbench": 0.25, "dsbench": 0.5})]
        results = self.summarize(programs, [{"fast": (0.020, 0.010), "slow": (0.010, 0.020)}] * 3)
        # finbench: 1 / (0.75 / 2 + 0.25 / 0.5); dsbench: 1 / (0.5 / 2 + 0.5 / 0.5).
        self.assertEqual(results["predicted"]["ry"]["finbench"],
                         {"median": round(1 / 0.875, 4), "low": round(1 / 0.875, 4), "high": round(1 / 0.875, 4),
                          "weight": 1.0, "programs": 2})
        self.assertEqual(results["predicted"]["ry"]["dsbench"]["median"], 0.8)
        self.assertEqual(results["geomean"]["ry"]["median"], 1.0)
        self.assertEqual(results["programs"][0]["calls"], 100)
        self.assertEqual(results["programs"][0]["weight"], {"finbench": 0.75, "dsbench": 0.5})

    def test_a_failed_program_leaves_the_prediction_and_its_weight_shows(self):
        programs = [self.program("fast", {"finbench": 0.75}), self.program("broken", {"finbench": 0.25})]
        rounds = [{"fast": (0.020, 0.010), "broken": (0.010, None)}] * 3
        state_rounds = self.summarize(programs, [{"fast": (0.020, 0.010), "broken": (0.010, 0.010)}] * 3)
        self.assertEqual(state_rounds["predicted"]["ry"]["finbench"]["weight"], 1.0)
        results = self.summarize(programs, rounds)
        self.assertEqual(results["predicted"]["ry"]["finbench"]["weight"], 0.75)
        self.assertEqual(results["predicted"]["ry"]["finbench"]["median"], 2.0)

    def test_compares_a_hot_checksum_sample_by_sample(self):
        self.assertTrue(bench.same_checksum("num:3:0:69.6585893385:0.50000000001,error", "num:3:0:69.6585893384:0.50000000000,error"))
        self.assertFalse(bench.same_checksum("num:3:0:69.7:0.50000000000,error", "num:3:0:69.6585893384:0.50000000000,error"))
        self.assertFalse(bench.same_checksum("num:3:0:2:0.50000000000,error", "num:3:0:2:-0.50000000000,error"))
        self.assertFalse(bench.same_checksum("num:3:1:2:1.00000000000,error", "num:3:0:2:1.00000000000,error"))
        self.assertFalse(bench.same_checksum("num:3:0:2:1.00000000000", "num:3:0:2:1.00000000000,error"))
        self.assertTrue(bench.same_checksum("chr:2:0:5:99", "chr:2:0:5:99"))

    def test_reads_a_region_profile(self):
        text = "\n".join([
            '{"region": "sd", "seconds": 0.1, "samples": 1000, "activities": {"interpreter": 600, "gc": 400}, "counters": {"calls": 7}}',
            '{"region": "", "seconds": 0.5, "samples": 5000, "activities": {"startup": 5000}, "counters": {}}', ""])
        self.assertEqual(bench.parse_regions(text), {"sd": {"seconds": 0.1, "samples": 1000,
                                                            "activities": {"interpreter": 0.6, "gc": 0.4},
                                                            "counters": {"calls": 7}}})

    def test_weights_a_breakdown_by_each_programs_share_of_ry_time(self):
        rows = [{"weight": {"finbench": 0.5}, "ratio": {"ry": {"median": 1.0}}, "breakdown": {"activities": {"gc": 1.0}}},
                {"weight": {"finbench": 0.5}, "ratio": {"ry": {"median": 0.5}}, "breakdown": {"activities": {"interpreter": 1.0}}}]
        # The second program takes twice the ry time of the first.
        self.assertEqual(bench.weighted_breakdown(rows), {"finbench": {"interpreter": round(2 / 3, 4), "gc": round(1 / 3, 4)}})

    def test_reads_a_callgrind_dump_its_region_triggered(self):
        dump = ("version: 1\ncmd: ry run\ndesc: Trigger: Client Request: stats-sd\nevents: Ir\nsummary: 0\n"
                "fn=(1) eval\n12 123456\ntotals: 123456\n")
        self.assertEqual(bench.parse_callgrind_dump(dump), ("stats-sd", 123456))
        self.assertIsNone(bench.parse_callgrind_dump("desc: Trigger: Program termination\nsummary: 0\ntotals: 99\n"))

    def test_a_function_holds_its_own_instructions_not_its_callees(self):
        dump = ("positions: line\nevents: Ir\n\n"
                "ob=(1) /lib/libR.so\nfn=(1) interp::eval::h0123456789abcdef\n0 100\n"
                "cob=(2) /lib/stats.so\ncfn=(2) C_cov\ncalls=1 0\n0 700\n+3 20\n\n"
                "ob=(2)\nfn=(2)\n0 700\n\n"
                "ob=(1)\nfn=(3) 0x00000000004a2b10\n0 5\nfn=(4) 0x00000000004a2c00'2\n0 6\n"
                "totals: 831\n")
        self.assertEqual(bench.parse_callgrind_functions(dump),
                         {("libR.so", "interp::eval"): 120, ("stats.so", "C_cov"): 700, ("libR.so", "(no symbols)"): 11})

    def test_function_shares_follow_the_programs_weights(self):
        programs = [self.program("a", {"finbench": 0.75}, gnu_seconds_per_call=1e-5),
                    self.program("b", {"finbench": 0.25}, gnu_seconds_per_call=1e-5)]
        counts = {"a": {"calls": 10}, "b": {"calls": 10}}
        by_function = {"a": {("libR.so", "eval"): 600, ("libR.so", "match_arguments"): 400},
                       "b": {("libR.so", "eval"): 1000}}
        self.assertEqual(bench.weighted_functions(programs, counts, by_function, 1),
                         [{"object": "libR.so", "function": "eval", "share": {"finbench": 0.7}}])

    def test_an_instruction_change_is_weighted_by_gnu_r_time(self):
        programs = [self.program("a", {"finbench": 0.5}, gnu_seconds_per_call=1e-5),
                    self.program("b", {"finbench": 0.5}, gnu_seconds_per_call=1e-4)]
        old = {"a": {"per_call": 1000.0}, "b": {"per_call": 10000.0}}
        new = {"a": {"per_call": 1500.0}, "b": {"per_call": 10000.0}}
        self.assertEqual(bench.weighted_instructions(programs, old), {"finbench": 100000000})
        changes = bench.compare_instructions(programs, old, new)
        self.assertEqual([change["id"] for change in changes], ["a", "b"])
        self.assertEqual(changes[0]["change"], 50.0)
        self.assertEqual(changes[0]["effect"], {"finbench": 25.0})
        self.assertEqual(changes[1]["effect"], {"finbench": 0.0})


class HotSelection(unittest.TestCase):
    def test_names_operators_and_dotted_names(self):
        self.assertEqual(hot.slug("base", "["), "base-subset")
        self.assertEqual(hot.slug("data.table", "rbindlist"), "data-table-rbindlist")
        self.assertEqual(hot.slug("base", "match.arg"), "base-match-arg")
        self.assertEqual(hot.slug("base", "names<-"), "base-names-assign")
        programs = [{"package": "base", "name": "a.b"}, {"package": "base", "name": "a_b"}]
        hot.assign_ids(programs)
        self.assertEqual([program["id"] for program in programs], ["base-a-b", "base-a-b-2"])

    def test_the_manifest_reads_back_as_written(self):
        manifest = {"id": "hot", "name": "Hot \"entries\"\n", "session": ["stats", "data.table"],
                    "selection": {"coverage": 0.9, "samples": 3},
                    "programs": [{"id": "x", "rng": [True, False], "iterations": [3, 4], "gnu_seconds_per_call": 2e-05,
                                  "share": {"finbench": 0.5}, "weight": {}}]}
        self.assertEqual(hot.tomllib.loads(hot.render_toml(manifest)), manifest)

    def test_keys_are_chosen_by_summed_share_until_each_code_base_is_covered(self):
        def capture(seconds):
            return {"entries": {key: {"seconds": value, "uncapturable_seconds": 0.0} for key, value in seconds.items()},
                    "samples": {key: [{}] for key in seconds if key != ("base", "lost")}}
        captures = {"finbench": capture({("a", "x"): 6.0, ("base", "lost"): 9.0, ("b", "y"): 3.0, ("c", "z"): 1.0}),
                    "dsbench": capture({("b", "y"): 9.0, ("c", "z"): 1.0})}
        # A key with no valid sample counts in its code base's time, so finbench never reaches 90%.
        self.assertEqual(hot.choose_keys(captures, 10, 0.9), [("b", "y"), ("a", "x"), ("c", "z")])
        self.assertEqual(hot.choose_keys(captures, 1, 0.9), [("b", "y")])
        self.assertEqual(hot.choose_keys(captures, 10, 0.45), [("b", "y"), ("a", "x")])

    def test_typical_samples_are_near_the_mean_then_small(self):
        samples = [{"seconds": 1e-3, "compact_bytes": 10, "path": "far"},
                   {"seconds": 1.02e-5, "compact_bytes": 900, "path": "near-big"},
                   {"seconds": 0.99e-5, "compact_bytes": 100, "path": "near-small"}]
        self.assertEqual([sample["path"] for sample in hot.typical_samples(samples, 1e-5, 2)], ["near-small", "near-big"])

    def test_gnu_r_time_scales_the_captured_time_by_the_samples_ratio(self):
        def capture(seconds, samples):
            return {"entries": {("p", "f"): {"seconds": seconds, "uncapturable_seconds": 0.0}}, "samples": {("p", "f"): samples}}
        slow = [{"seconds": 2e-3, "gnu_seconds_per_call": 1e-3}, {"seconds": 6e-3, "gnu_seconds_per_call": 3e-3}]
        captures = {"finbench": capture(10.0, slow), "dsbench": capture(4.0, [])}
        # dsbench has no valid sample of its own, so it takes the ratio of every code base's.
        self.assertEqual(hot.gnu_time(("p", "f"), captures, slow), {"finbench": 5.0, "dsbench": 2.0})
        # The entry's own time in calls the recorder could not capture counts as well.
        captures["dsbench"]["entries"][("p", "f")]["uncapturable_seconds"] = 2.0
        self.assertEqual(hot.gnu_time(("p", "f"), captures, slow), {"finbench": 5.0, "dsbench": 3.0})

    def test_a_weight_is_the_share_of_gnu_r_time(self):
        programs = [{"id": "a", "gnu_time": {"finbench": 3.0}}, {"id": "b", "gnu_time": {"finbench": 1.0, "dsbench": 2.0}}]
        hot.weigh_programs(programs, "finbench")
        hot.weigh_programs(programs, "dsbench")
        self.assertEqual([program["weight"] for program in programs], [{"finbench": 0.75}, {"finbench": 0.25, "dsbench": 1.0}])

    def test_tracking_compares_each_configuration_with_the_first(self):
        programs = [{"id": "a", "weight": {"finbench": 0.75}}, {"id": "b", "weight": {"finbench": 0.25}}]
        measured = {"hot": {"gnu_r": {"a": 1.0, "b": 1.0}, "new": {"a": 0.5, "b": 1.0}, "old": {"a": 1.0, "b": 2.0},
                            "broken": {"a": 1.0}},
                    "codebases": {"finbench": {"gnu_r": {"seconds": 100.0}, "new": {"seconds": 50.0, "checksum": "ok"},
                                               "old": {"seconds": 100.0, "checksum": "ok"},
                                               "broken": {"seconds": None, "reason": "crashed"}}}}
        new, old, broken = hot.tracking_rows(measured, programs, ["new", "old", "broken"], "finbench")
        self.assertEqual((new["predicted"], new["measured"], new["predicted_change"]), (0.625, 0.5, 0.0))
        self.assertEqual((old["predicted"], old["measured"]), (1.25, 1.0))
        self.assertAlmostEqual(old["predicted_change"], math.log(2))
        self.assertAlmostEqual(old["measured_change"], math.log(2))
        # A program the configuration did not run leaves the prediction to the others' weight.
        self.assertEqual((broken["predicted"], broken["weight"], broken["measured_change"]), (1.0, 0.75, None))

    def test_a_configuration_names_a_binary_and_its_environment(self):
        self.assertEqual(hot.parse_configuration("nojit=/opt/ry,RY_JIT=off,RY_BASE_R=/base"),
                         ("nojit", "/opt/ry", {"RY_JIT": "off", "RY_BASE_R": "/base"}))
        with self.assertRaises(SystemExit):
            hot.parse_configuration("/opt/ry")

    def test_correlation_needs_three_changes(self):
        self.assertEqual(hot.correlation([(0.1, 0.1), (0.2, 0.2)]), (None, None))
        r, slope = hot.correlation([(0.1, 0.05), (0.2, 0.1), (-0.2, -0.1)])
        self.assertAlmostEqual(r, 1.0)
        self.assertAlmostEqual(slope, 0.5)


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
