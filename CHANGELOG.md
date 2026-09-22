# Changelog

All notable changes to ry are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

This file starts from the type-checker work; earlier history lives in the git log.

## [Unreleased]

### Added

- **Static type checker for R.** A Hindley–Milner core with union-find inference, numeric
  constraints, and prenex generics, checking R annotated in `#:` comments (a JSDoc-like notation that
  keeps annotated code compatible with ordinary R tooling). Type-error diagnostics are opt-in via
  `[check] typing`; hover types, inlay hints, and signature help are on by default. The typing
  semantics are specified in `docs/typing-reference.md`.
- **Standard-library type stubs.** Declaration-only `.Rtypes` stub files (`name : <type-expr>`) for the
  base/stats/utils/methods libraries, compiled into the binary and overridable per project
  (`stubs/*.Rtypes` win over the shipped set). Parametric higher-order functions (`lapply`, `Map`,
  `Reduce`, …) carry real generics.
- **Type-annotation syntax extensions**: variadic rest parameters (`fn(...: T)`) and dotted parameter
  names (`na.rm`).
- **Semantic-token highlighting** for the `#:` type notation in `.R` files.
- Editor tooling: hover, completion, goto-definition, references, rename, document/workspace symbols
  (including S4 `setClass`/`setGeneric`/`setMethod` and R6 classes), inlay hints, and an unused-local
  lint.

### Changed

- **Renamed from Roughly to ry.** The binary is `ry`, the config file is `ry.toml`, suppression
  comments are `# ry: allow(...)`, environment variables are `RY_*`, and the editor settings live
  under `ry.*`. **Nothing needs changing to upgrade:** `roughly.toml`, `# roughly: allow(...)`,
  `ROUGHLY_*` and the `roughly.*` settings are all still honoured, and the REPL history directory is
  moved for you. The crate is published as `ry-lang`, since `ry` was already taken on crates.io.
- **`ry check` renders its findings with [miette](https://github.com/zkat/miette).** Every message
  the CLI prints — findings, companion locations, and configuration failures alike — goes through
  one graphical reporter: the diagnostic code heads the report, the source snippet is drawn with
  the reported range underlined, and a companion location is drawn nested under the finding from
  its own file. A malformed `ry.toml` is shown in place rather than described. Colour and rules
  follow the destination (a terminal gets unicode and colour, `NO_COLOR` and pipes get plain
  ASCII). The `--output json` records are unchanged.
- Analysis runs on a single in-house red–green memoized query engine with latest-edit-wins
  cancellation; per-edit output is verified byte-identical to a from-scratch rebuild.
- The R grammar tracks the published `tree-sitter-r` 1.3.0.

### Fixed

- **The prebuilt Linux binary runs on any x86_64 distribution.** It used to load its C library
  from a Nix store path, so it did not start outside Nix. It is now statically linked against musl
  and published as `ry-x86_64-unknown-linux-musl.tar.gz` (formerly
  `ry-x86_64-unknown-linux-gnu.tar.gz`); the Zed extension looks for the new name. A static binary
  cannot load R, so `ry repl` and `ry run` need ry built from source on Linux.

[Unreleased]: https://github.com/felix-andreas/ry/compare/main...HEAD
