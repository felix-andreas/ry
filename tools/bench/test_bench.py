#!/usr/bin/env python3
"""Tests of bench.py's release-archive mode: what results.json records about the archive, and which
images the run accepts.

A fake archive stands in for a release: its `ry` is a shell script that, like the real one,
extracts a runtime library (a C library built here that exports `ry_runtime_id`) and a base R tree
into the cache directory on its first start, writes an R home whose `.libr` names that library, and
prints `R.home()`. crates/ry/tests/test_release_metadata.rs runs this file.

usage: python3 -I tools/bench/test_bench.py
"""
import hashlib, importlib.util, io, os, pathlib, shutil, subprocess, sys, tarfile, tempfile, unittest
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


def start_image(identity):
    """A start-up image header: the magic, the header's length, then the identity with its length."""
    name = identity.encode()
    return b"RYSTART\0" + (16 + len(name)).to_bytes(8, "little") + len(name).to_bytes(8, "little") + name


def code_image(identity):
    name = identity.encode()
    return b"RYCODE\0\0" + len(name).to_bytes(8, "little") + name


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
        source.write_text(f'const char *ry_runtime_id(void) {{ return "{IDENTITY}"; }}\n')
        subprocess.run(["cc", "-shared", "-fPIC", "-o", payload / "libR.so", source], check=True)
        (payload / "library/base/R/ry-start.image").write_bytes(start_image(IDENTITY))
        (payload / "library/stats/R/stats.ryc").write_bytes(code_image(IDENTITY))
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
        foreign = self.corpus_library / "zoo/R/zoo.ryc"
        foreign.parent.mkdir(parents=True)
        foreign.write_bytes(code_image("otherbuild"))
        with self.assertRaises(SystemExit) as refusal, redirect_stdout(io.StringIO()):
            bench.refuse_foreign_images([ry])
        self.assertIn(f"{foreign} (otherbuild)", str(refusal.exception))
        self.assertIn(f"runtime {IDENTITY}", str(refusal.exception))


if __name__ == "__main__":
    unittest.main()
