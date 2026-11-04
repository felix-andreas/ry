---
title: Typed ry's hill climb
description: How typed ry grows: a fitness function over speed, friendliness and soundness, and the log of each step
---

Typed ry grows by a hill climb. Each step is one change to the language or its implementation, and
it is kept only when it raises the fitness without lowering soundness. `just climb` computes the
fitness; `just climb --log "NOTE"` also appends it to `tools/climb/log.tsv`, which is the climb's
step log. The tour of what exists is [Typed ry](/reference/language).

## Fitness

| Score | Measures | Better |
| --- | --- | --- |
| `unsound` | Seeded random programs that strict mode accepts but that fail with a type error, or hold a value of another type than declared, when they run | 0, always |
| `rejected` | Random well-typed programs that strict mode rejects | lower |
| `friendly` | Cases in `tools/climb/friendly/` that behave as their first line says | higher |
| `suffixes per 100 lines` | `i64`/`f64` suffixes the typed benchmark programs need | lower |
| `typed ry / Rust` | Geometric mean of typed ry's time over the Rust reference's, per benchmark program | lower, 1 is Rust |
| `untyped ry / GNU R` | The same for the untyped twins on ry against GNU R | lower |

Soundness is a constraint, not a trade: a step that makes `unsound` nonzero is reverted or fixed
before anything else. Among the other scores, speed and friendliness weigh equally.

## Corpus

- **Speed.** The 17 programs in `tools/bench/why/typed/` (typed ry, strict mode) and
  `tools/bench/why/untyped/` (plain R), each with a Rust reference in `tools/climb/rust/` written
  the way a Rust programmer would and printing the same checksum.
- **Friendliness.** `tools/climb/friendly/*.R`. A case starts with `# expect: ok` or
  `# expect: CODE`. An `ok` case must pass strict mode and, on ry's runtime, print exactly its `#>`
  lines; any other case must report exactly that one diagnostic code. A known gap gets a case first,
  so it fails until a step fixes it.
- **Soundness.** A generator in `tools/climb/run.py` writes programs over `I64`, `F64`, `Bool`,
  `Str` and their lists that lean on unsuffixed literals, typed and untyped bindings, `if`, `match`
  arms, generic standard library calls and lambdas. Each program prints the `typeof()` of every
  value, which must match its declared type (`I64` for `I64` and `List<I64>`, `double` for `F64`).
  `--seed` and `--programs` replay or widen a run, and a failure prints the program's path.

## Steps

`tools/climb/log.tsv` holds one line per measured step: the date, the commit, every score, and a
note. A taste question a step raises (a new syntax, a changed default) goes to the orchestrator in
one batch rather than being decided inside the climb.
