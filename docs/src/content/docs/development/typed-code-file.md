---
title: The typed code file
description: "How an installed package carries the machine code of its typed functions: position-independent code mapped from one file per package, a per-process offset table, attachment per function at its first call, the file format and the tests"
---

`ry install` compiles a package's typed functions and writes their machine code into the installed
package as `R/<package>.<build>.ryt`, beside the lazy-load database and the bytecode image. A later
session maps that file, and the first call of a typed function runs code compiled at install
instead of compiling it. `crates/typed/src/file.rs` is the writer and the loader, and this page is
their contract.

```console
$ ry install rates -l library
...
** typed code: closures=2 written=2
** code image: closures=2 written=2
* DONE (rates)
$ RY_TYPED_LOG=1 ry run use.R      # library(rates, lib.loc = "library"); print(total(1000, 0.001))
typed: attached `total` for [I64, Dbl] -> Dbl from rates's code file
[1] 718640.9
```

`ry build --aot` on a package does what `ry install` does. `R CMD INSTALL --ry-no-typed-code` skips
the file, as `--ry-no-code-image` skips the bytecode image, and the two options are independent.
A typed function that the file does not supply compiles at its first call, as it would without
the file, so a missing, stale or damaged file costs time and never changes what a program prints.

## Measured

On an iterate build, one Linux machine:

| Measure | Target | Measured |
|---|---|---|
| First calls of 50 typed functions of one package | | 1.43 ms from the file, 18.5 ms compiled in the session |
| Attaching one function at its first call | at most 20 µs | about 21 µs, including the lazy-load fetch of the closure |
| Peak resident memory over `library()` alone, same 50 calls | | 2.2 MiB from the file, 5.0 MiB compiled in the session |
| `library(pkg)` with a code file | at most 0.1 ms slower | 7.13 to 7.27 ms with the file, 7.51 ms without |
| Code pages of two processes that attached the same file | shared | `Shared_Clean` in both, one copy in memory |
| Warm time of file code over session code (same package) | within 2% | orders 1.010, report 1.014, sales 0.975, drawdown 1.006, vasicek 0.996 |
| Typed benchmark functions left out for a `match` over any tag | at most a tenth | 9 of 170 (5.3%) |

The file holds every function that `ry run --aot` compiles up front for the same program, except
those left out for a `match` over any tag: `pidigits` 18 of 18, `spectralnorm` 5 of 5, `nbody_2`
5 of 5, `orders` 1 of 3.

## What the file holds

The file holds one compiled version per pair of a closure and its argument types, which is the key
of the session's table of compiled code (`COMPILED` in `crates/typed/src/typed.rs`). The roots are
the package's typed closures whose parameter annotations say which values arrive from R, compiled
for those types, and the file also holds every typed function of the same database that a root
calls, for the types the caller passes.

The install log counts the typed closures, the versions written, and the versions left out by
reason:

| Reason in the log | What it means |
|---|---|
| `compiles with its caller` | a parameter type R cannot pass, such as a record, so the closure is not a root; it is in the file when a root calls it |
| `does not compile` | the typed core refuses the function, so it runs in the interpreter in every session |
| `matches over any tag` | its code tests for the shapes the installing process had confirmed, which depends on the process |
| `assumes a lookup a session cannot check` | it resolved a name in an environment the file cannot name |
| `calls R with code a session cannot rebuild` | an R call site holds a value the file cannot store |
| `calls a function it cannot name`, `has a type a file cannot name` | the same for a callee and for a type |
| `does not compile into a file`, `calls code that did not link` | the code needs a relocation the writer does not resolve, or calls such code |

A closure copied at run time (an R6 method bound by `$new()`, a function after `environment<-`, the
per-class copy of an S4 method) is another closure with another environment, so it compiles in the
session, and the closure it was copied from still attaches. The versions a `--train` workload
compiles are not in the file.

## Position-independent code

Session code embeds process addresses as 64-bit immediates. File code instead loads every word that
differs between processes from an offset table that sits directly below the code, so the code bytes
are the same in every process and are never written after they are mapped. The backend emits both
through one seam (`Slot` in `crates/typed/src/backend.rs`): in the session a slot's word is an
immediate, and in a file it is a table load.

| Slot kind | Filled with | When |
|---|---|---|
| `Helper(id)` | the address of runtime helper `id`, or of a libcall such as `fmod` | when the file is mapped |
| `Interrupt` | the address of `interp::unwind::INTERRUPT_REQUESTED` | when the file is mapped |
| `Shape(entry)` | this process's id for the file's shape `entry`, after interning it | when the file is mapped |
| `Constant(entry)` | the static box of that shape (`shapes::constant`) | when the file is mapped |
| `Data(offset)` | the address of the file's read-only data plus `offset`, which holds static strings | when the file is mapped |
| `Site(index)` | a `*const RCall` built for R call site `index` | when its function attaches |
| `Callee(index)` | the entry of a typed function of another namespace | when its function attaches |

Slot `n` is the 8-byte word at `base - 8 * (n + 1)`, where `base` is the start of the code. Helper
ids are positions in one static array of helper addresses, so a file is only valid for the ry build
that wrote it, which the header checks. A shape id never appears in the code as an integer: lowering
writes `Constant::Shape`, and the backend turns it into a `Shape` slot.

`ssa::Relocatable` (`crates/ssa/src/lower.rs`) compiles the file's functions. Lowering to Cranelift
IR runs on the main thread, because it reads the typed core's thread-local shape table, and
`Relocatable::finish` compiles the IR on worker threads, lays the functions out in declaration
order and resolves every relocation: a table load against its slot, a libcall against a `Helper`
slot, and a call between two functions of the file as a direct relative call. A function with any
other relocation is left out.

## File format

All integers in the header are little-endian `u64`s after the magic and a `u32` version.

| Part | Contents |
|---|---|
| Header | magic `RYTYPED\0`, format version 1, then the hash of the build identity, the `.rdb`'s length and modification time, the hash of the code target (triple and ISA flags), the metadata's length, the number of slots, the code's offset, the code's length, the data's length, where the shared code starts, the metadata's hash, the shared part's hash, and a reserved zero word |
| Metadata | LEB128 varints, in this order: `strings`, `shapes`, `slots`, `lookups`, `sites`, `callees`, `functions` |
| Padding | zeros up to the code's offset, a multiple of 64 KiB |
| Code | the function bodies and their trampolines, then the code every function shares (the free entries) |
| Data | after the code, padded to 8 bytes: the read-only data that `Data` slots address |

The metadata tables are:

- **`functions`**: per version, sorted by (record offset, position, signature): the binding name for
  the log, the closure's `.rdb` record offset and its position in that record, its argument and
  result types, the offsets of its body and trampoline, where its code ends and that code's hash,
  whether it calls R, and its lists of lookups, sites, callees and confirmed shapes.
- **`lookups`**: per name a version resolved when it compiled: the symbol, whether a function was
  looked up, the environment the lookup starts from and the one it found the binding in, and the
  class of what it found (below).
- **`sites`**: per R call site, every field of its `RCall`: the environment, the call expression, the
  called function's lookup, per argument its tag, code, type and conversion, how the result is used,
  and the flags `diverges` and `effect_free`.
- **`callees`**: per typed function a version calls, its lookup, the signature the caller compiled
  against, and whether it is a version of this file (a direct call) or reached through a `Callee`
  slot.
- **`shapes`**: per record layout, tag union and list item type that the code or a signature names,
  enough to intern it in another process. An entry names only entries before it.
- **`slots`** and **`strings`**: the slot kinds above, and the bytes of every name.

A closure is named by the offset of its lazy-load record in the package's `R/<package>.rdb` and its
position among the closures of that record, in the order `serialize::unserialize_with_closures`
yields them. The `.rdb` stamp in the header ties those offsets to one database. `sysdata.rdb` is
never named.

A lookup records what lowering found, because lowering specializes on more than a function's
identity, and attaching requires the same class:

| Class | What lowering did with it | What attaching requires |
|---|---|---|
| `File` | a direct call, or the function inlined | a typed closure noted from the same package, record, position and database stamp |
| `Base` | code for the builtin by name | `is_base_function` holds for what the name finds |
| `Table` | a call that gives a table value | `is_table_function` holds |
| `Library` | the standard library constant inlined | the name finds the standard library's own binding |
| `Other` | a call through a `Site` slot | a function that is not typed |

An environment is named as the global environment, base, base's namespace, the standard library, a
package's namespace, its imports, or an attached package, so a later session finds its own.

## Writing

The install session that writes the bytecode image (`crates/rcmd/src/code-image.R`, run by
`write_code_image` in `crates/rcmd/src/rcmd.rs`) loads the namespace, runs the training workload if
one was asked for, and calls `ryTypedCodeWrite(package)` before `ryCodeImageWrite`, because writing
the image ends the recording of the database's closures that the code file names. The writer:

1. Takes every typed closure the install recorded from the package's database, with its record
   offset and position.
2. Compiles each root for its declared types, and repeats the pass while it confirms new shapes,
   as `ry run --aot` does for a script.
3. Lowers every version the roots reach through calls of the same database again for the file
   target, leaves out what the table under "What the file holds" lists, and encodes its lookups,
   sites and callees.
4. Compiles, lays out and links, drops a version whose direct callee was dropped, and writes the
   file to `<name>.ryt.partial` before it renames it into place, so a process never maps a
   half-written file.

Two writes against one installed package give the same bytes. `images::prune` removes the `.ryt`
files of other builds as it removes their `.ryc` files.

## Loading

**Noting closures.** When a session first reads from a package's `.rdb`, it checks once whether a
code file of its own build lies beside it. If one does, every lazy-load fetch from that database
goes through `unserialize_with_closures` and notes each typed closure's record offset, position and
the database's stamp at that first check (`code_image::typed_note`). A noted closure is preserved
for the rest of the session, so its address never names another closure. A database without a
file is read as before and notes nothing, so an untyped package pays nothing.

**Opening.** The first miss in `compiled_or_refusal` for a noted closure opens the file. It reads
the header and the metadata, and the file is refused when the magic, version, build or target
differ, when the header's stamp differs from the note's (a reinstall in a running session replaced
the database while the notes still name the old one), when the file's length disagrees with the
header, or when the metadata's hash or any index in it is wrong. Parsing is a pure function over
bytes (`parse`), so it is fuzzed without a session. A refusal prints one `RY_TYPED_LOG` line and
leaves the package's functions to compile in the session.

**Mapping.** The loader reserves one range for the table and the code, makes the table writable,
and on Linux maps the code and data from the file read-only and executable over the upper part.
Where that mapping is refused, and on every other Unix, it maps fresh memory at the same place,
reads the code and data into it, makes it executable and flushes the instruction cache; the table
keeps its fixed distance and only the sharing is lost (`RY_TYPED_FILE_COPY=1` forces this path).
Windows does not use the file. The loader checks the hash of the shared code and data, interns every
entry of `shapes`, and fills the slots that do not wait for a function. A mapping lasts for the life
of the process, because static strings point into it and frames of its code may be on the stack
when a namespace unloads.

**Attaching.** A version is attached when its closure is first called with its argument types, and
attaching builds the same `Compiled` entry a session compile would:

1. Find the version by the closure's note and the argument types. No version is a miss.
2. Check the hash of the version's code.
3. Resolve each lookup in the live session and require the class it recorded.
4. Build each R call site's `RCall` from its recorded fields and fill its slot.
5. For each callee: a version of this file attaches first, depth first, and must be noted from the
   same record and position; a typed function of another namespace attaches or compiles through
   `compiled_or_refusal`, and its entry fills the `Callee` slot. Its result type must match the
   signature the caller compiled against.
6. Confirm the shapes the version constructs (`shapes::confirm`), as a session compile does.
7. Insert the entry into `COMPILED` and root the closure. The entry's speculations are its own
   lookups and those of every callee, and its epoch is the earliest of theirs.

Any failure marks the version as failed, prints one `RY_TYPED_LOG` line with the reason, and leaves
the closure to compile in the session. A version whose direct callee is still being attached
misses, so mutually recursive functions compile in the session; a function that calls itself
attaches. An attached entry holds no JIT memory, so it does not count toward `MAX_COMPILED`. Every
later call checks the entry's speculations as it does for session code, so a rebinding after
`unlockBinding` drops the entry the same way. `RY_AHEAD_MISS=1` makes every attach miss, which the
tests of the fallback use.

**A file from another build.** Under `ry run --aot`, a namespace that loads with typed closures and
a `.ryt` file of another build compiles its typed functions as it loads, and prints one line:

```console
$ ry run --aot use.R
ry: another build of ry installed rates, so its typed functions compiled as it loaded; reinstall it to compile them at install
[1] 718640.9
```

A package with no file at all is not compiled at load, because telling an untyped package from one
installed with `--ry-no-typed-code` would mean reading every function.

## Trust

A code file is machine code that the process runs, like the shared objects in a package's `libs`
directory, and it is trusted to the same degree: whoever can write the installed package can run
code in the session. The hashes and stamps catch truncation, a file from another build and a
database that changed, not an attacker.

## Tests

| Test | What it pins |
|---|---|
| `typed_scripts_agree_however_they_compile` installs every generated typed script as a package and runs it with its file, then with `RY_AHEAD_MISS=1` | file code prints what every other tiering prints, and the fallback does too |
| `install-typed-package.scenario` | the attach line; `RY_GC_STRESS` and the copy path over attached code; `ry build --aot`; a file from another build under `ry run --aot`; a `match` over a closed union, a record parameter and a sort comparator attached after the process interned other shapes; a closure copied at run time; a reinstall in a running session |
| `fuzz_typed_code_parse` (`crates/typed/tests/test_file.rs`, `cargo fuzz` target `typed_code_parse`) | random valid metadata reads back as written, and truncated or bit-flipped bytes never panic the parser |
| `fuzz_typed_code_file` (`crates/ry/tests/test_scenarios.rs`) | an installed file damaged in its header, metadata or code never attaches, and a second write against one install gives the same bytes |

`ry build --aot --exe main.R`, which appends a script's code file to a copy of the `ry` executable,
is not built (`backlog.md`, "Open: ahead-of-time compilation").
