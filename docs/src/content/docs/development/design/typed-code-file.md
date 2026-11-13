---
title: The typed code file
description: "Design draft for ahead-of-time compiled typed code in an installed package: position-independent machine code mapped read-only from one file per namespace, a per-process table as its only private memory, and attachment per function at its first call"
---

This draft designs the build output behind `ry build --aot` and `ry install`: one file of machine
code per installed namespace, written at install and mapped by every process that calls the
package's typed functions. It is the implementation plan for the "Mapped, shared code" paragraph
of [the typed runtime](/development/design/typed-runtime/), and the ordered work list is in
`backlog.md` under "Open: ahead-of-time compilation".

`ry run --aot main.R` exists and compiles a script's typed functions before the script starts
([CLI reference](/reference/cli#compiling-ahead-of-time)). It moves the compile earlier and does
not remove it. The code file removes it: a typed function of an installed package runs its first
call on code that was compiled when the package was installed.

## The numbers to beat

A prototype of install-time typed code against an older image format measured, on a generated
package, 50 first calls in 1.0 ms from the image against 40.1 ms compiled in the session, and
1.8 MiB of added peak resident memory against 5.9 MiB. `ry run --aot` on the typed benchmark
programs spends 1.1 to 5.9 ms per program compiling up front (50.6 ms for `pidigits`), which is
the cost a code file takes out of a process.

| Measure | Target | Why this number |
|---|---|---|
| Attaching one function at its first call | at most 20 µs | the prototype's 1.0 ms over 50 first calls |
| Compile work in a process whose functions are all in the file | zero `typed: compiled for` lines | the directive: nothing compiles mid-run |
| Warm time of file code over code compiled in the session | within 2% (geometric mean, typed suites) | same compiler, same IR: only address loads differ |
| `library(pkg)` with a code file over without one | at most 0.1 ms more | the file is opened at the first typed call, not at load |
| Proportional set size of the code pages with four processes | about a quarter of the resident size | the pages are shared, only the table is private |

## What the user sees

```console
$ ry install pkg -l library
$ RY_TYPED_LOG=1 ry run use.R        # use.R calls library(typedpkg); total(1000, 0.001)
typed: attached `total` for [I64, Dbl] -> Dbl from typedpkg's code file
[1] 718640.9
```

`ry install` writes the code file whenever the package has typed functions, and `ry build --aot`
on a package directory does what `ry install` does (it accepts `-l`). `--ry-no-typed-code` on the
underlying `R CMD INSTALL` skips the file, as `--ry-no-code-image` skips the bytecode image. A
package without the file, or with a file from another ry build, behaves as it does today: its
typed functions compile at their first call, and under `ry run --aot` they compile when the
namespace loads, with one line that names the package and says to reinstall it.

## What the file holds

The file holds one compiled version per pair of a closure and its parameter types, which is the
key of the session's table of compiled code (`COMPILED` in `crates/typed/src/typed.rs`). Three
sources fill it.

- **Declared roots.** Every typed closure of the namespace whose parameter annotations say which
  values arrive from R, compiled for those types, and every typed function such a root calls,
  for the types the caller passes. This is the pass `typed::ahead::compile` runs for a script,
  given the namespace's bindings as its definitions.
- **Training instantiations.** With `ry install --train` the install session runs the workload
  before it writes, so the table also holds the versions the workload compiled: a function with a
  record or function parameter for the shapes it was called with, and a function whose
  `Num.from_r` or unchecked `List.from_r` took its type from a call. The file stores those too.
  They are guesses, so they wait for the rule that a compiled guess which fails falls back
  instead of stopping (work list, step 2).
- **Nothing else.** A function the lowering refuses is not in the file and runs as it does today.
  A function with a `match` over any tag is left out of the first version, because its code
  tests for the shapes the installing process had confirmed, and a process with other typed
  packages loaded has confirmed more ("Open questions").

## Position-independent code

The backend embeds process addresses as 64-bit immediates today: every runtime helper call is
`iconst(address)` followed by `call_indirect` (`call` and `call_words` in
`crates/typed/src/backend.rs`, 102 uses of the `address!` macro), and so are typed callees
(`call_typed`), R call sites (`call_r`), the free stubs (`preserving`, `count`), static strings
and static boxes (`Constant::Str`, `Constant::Object`), shape ids (`construct`) and the interrupt
flag (`Op::Poll`). None of those values is the same in another process.

File code loads each such word from a table instead. Cranelift already emits that load: with
`is_pic` set, `symbol_value` of an external name that is not colocated compiles to
`mov name@GOTPCREL(%rip), %reg` on x86-64 (relocation `X86GOTPCRel4`) and to `adrp` plus `ldr` on
AArch64 (`Aarch64AdrGotPage21`, `Aarch64Ld64GotLo12Nc`). The table is that global offset table.
Each word is one external name, numbered by its slot, and the writer resolves each relocation to
the slot's distance from the instruction, which is fixed because the table is mapped at a fixed
distance before the code. A call between two functions of the file is a direct relative call
(`X86CallPCRel4`, `Arm64Call`). No relocation is left in the file, so its code pages are never
written and every process shares them.

Two alternatives were rejected. Patching addresses into the code at load makes each patched page
private to its process, which gives up the sharing. A table pointer in a pinned register costs
every typed function a register and every entry from R a write to it, where the relative load
costs neither.

The table has these slot kinds, and the file's `slots` column says what to put in each.

| Slot kind | Filled with | When |
|---|---|---|
| `Helper(id)` | the address of runtime helper `id` | when the file is mapped |
| `InterruptFlag` | the address of `interp::unwind::INTERRUPT_REQUESTED` | when the file is mapped |
| `Shape(index)` | the process's id for the file's shape `index`, after interning it | when the file is mapped |
| `Constant(index)` | the static box of the file's shape `index` (`shapes::constant`) | when the file is mapped |
| `Site(index)` | a `*const RCall` built for R call site `index` | when its function attaches |
| `Callee(index)` | the entry address of a typed function in another namespace | when its function attaches |

Helper ids are positions in one static array of the helpers' addresses, which replaces the
`address!` uses, so a helper is named once. The build stamp makes the numbering safe: a file is
read only by the ry build that wrote it. Static string bytes live in the file's read-only data
and are addressed relative to the instruction, so a string constant needs no slot. That is sound
because a mapped file is never unmapped (see "Loading").

Session code keeps its immediates. Both targets go through one seam in the backend, an
`Address` that is either an absolute word or a slot, so the difference is confined to `call`,
`call_words`, `call_typed`, `call_r`, `preserving`, `count`, the two constant arms, `construct`
and `Op::Poll`. Lowering becomes target-independent: `Constant::Str` carries the string, not a
pointer from `ry_str_static`, and `Constant::Object` carries the shape, not a box address.

`crates/ssa` gains what the file target needs and nothing in its existing lowering changes:

- an IR instruction `table_word(slot)` that lowers to the `symbol_value` above, and a direct call
  to a function of the same object;
- `ssa::Object`, a module kind beside the JIT module: it compiles each function with
  `cranelift_codegen::Context::compile` on an ISA built with `is_pic`, keeps the bytes and the
  relocations (`MachBufferFinalized::relocs`), lays the functions out and resolves the four
  relocation kinds named above. It is about 300 lines and needs no new dependency.

Compiling one function in an `ssa::Object` touches no shared state, so the writer lowers on the
main thread, which reads R objects, and compiles on worker threads. It lays the results out in
the order of the `functions` column, so the file's bytes do not depend on thread timing.

## File format

The file is `R/<package>.<build>.ryt`, beside the bytecode image `R/<package>.<build>.ryc`
(`rcmd::images`). All integers are little-endian, every offset is a `u32` from the start of the
file, and every table is a struct of arrays: one array per field, all of the same length.

| Part | Contents |
|---|---|
| Header | magic `RYTYPED\0`, format version, the build identity, the length and modification time of the package's `.rdb` (the bytecode image's `DatabaseStamp`), a hash of the target triple and ISA flags, the section directory, and a hash of every section except the code |
| `functions` | per compiled version, sorted by (record, position, signature): the `.rdb` offset of the closure's lazy-load record, the closure's position in that record, its signature, the code offsets of its body and its trampoline, its code length and code hash, and its ranges in `lookups`, `sites` and `callees` |
| `signatures` | parameter types and result type of each version, in the encoding of `ir::Type` |
| `lookups` | per name a version resolved when it compiled (a `Speculation`): the symbol, where the lookup starts, and what it must find, which is a function of this file by index or any R function |
| `sites` | per R call site: the path from the closure's body to the call expression, as child indices |
| `callees` | per call of a typed function in another namespace: its lookup and the signature the caller compiled against |
| `slots` | per table word: its kind and payload, as in the table above |
| `shapes` | per shape the code names: its layout, by field names, and its item types |
| `strings` | the bytes of symbols and field names |
| Code | page-aligned at a multiple of 64 KiB: function bodies, trampolines, the free stubs and static string data |

A closure is named by the offset of its lazy-load record in the `.rdb` and its position among the
closures of that record, in the order `serialize::unserialize_with_closures` yields them. The
`.rdb` stamp in the header ties those offsets to one database. The bytecode image names closures
the same way.

## Writing

The install driver that writes the bytecode image (`crates/rcmd/src/code-image.R`, run by
`write_code_image` in `crates/rcmd/src/rcmd.rs`) loads the namespace, runs the training workload
if one was asked for, and writes the image. The code file is written by the same session, after
the image, through a builtin `ryTypedCodeWrite(package)`:

1. Force every binding of the namespace, recording each closure's record offset and position.
2. Run the up-front pass over the namespace's closures (`typed::ahead::compile`, with the
   bindings as its definitions).
3. For every successful entry of `COMPILED` whose closure belongs to the namespace, lower it again
   for the file target and collect its lookups, sites and callees.
4. Compile on worker threads, lay out, resolve relocations, and write the file to a temporary
   name in the same directory, then rename it into place.

Writing the same package twice with the same ry build gives the same bytes. A test pins that.

## Loading

**Finding a closure's code.** `do_lazy_load_fetch` (`crates/interp/src/serialize.rs`) knows the
record offset it reads, and the unserializer knows each closure's position. For a namespace whose
package has a code file, it notes each typed closure's (record, position) under the closure's
body, with the lifetime rule the pending bytecode has (`note_pending` in
`crates/interp/src/code_image.rs`). A package without the file notes nothing, so untyped packages
pay nothing.

**Opening.** The first miss in `compiled_or_refusal` for a closure with a note opens the file:
it reads the header, checks the magic, version, build identity, `.rdb` stamp and target hash,
bounds-checks every section and index, and checks the metadata hash. Any failure marks the
namespace as having no file, with one `RY_TYPED_LOG` line that says why. Parsing is a pure
function over bytes (`parse(bytes, build, stamp, target)`), so it is fuzzed without a session.

**Mapping.** The loader reserves one address range for the table and the code, maps the file's
code part over the upper part as read-only and executable, and leaves the table as private
writable memory below it. It fills the `Helper`, `InterruptFlag`, `Shape` and `Constant` slots.
A mapping lasts for the life of the process: static strings point into it, and frames of its
code may be on the stack when a namespace unloads. Where the system refuses an executable
mapping of file pages, the loader copies the code part into the memory the JIT uses for its own
code. That keeps the zero compile and gives up the sharing, and it is the path for every
platform until the mapping is verified there.

**Attaching.** A function is attached when it is first asked for, and attaching builds the same
`Compiled` entry a session compile would:

1. Find the version by (record, position, parameter types). No version is a miss: the function
   compiles in the session.
2. Check the code hash of the version.
3. Resolve each lookup in the live session, as `still_resolves` does for session code. A lookup
   that must find a function of this file must find the closure with that (record, position). A
   lookup that finds something else rejects the version.
4. Build the `RCall` of each site from the closure's body and the resolved function, and fill
   its slot. Resolve each callee in another namespace through `compiled_or_refusal`, which
   attaches or compiles it, check its result type against the signature the caller compiled
   against, and fill its slot.
5. Attach every function of this file the version calls directly, depth first, with a state per
   version (unattached, attaching, attached, rejected) so that recursion ends. A rejected callee
   rejects the caller, because the caller's direct call would run code that is not valid.
6. Insert the entry into `COMPILED` with the resolved speculations.

A rejected version compiles in the session, so a rejection costs time and never changes what the
program does. Every later call checks the entry's speculations as it does for session code, so a
rebinding after `unlockBinding` drops the entry the same way.

## Trust and corruption

A code file is machine code that the process runs, like the shared objects in a package's `libs`
directory, and it is trusted to the same degree: whoever can write the installed package can run
code in the session. The hashes and stamps are there to catch truncation, a file from another
build and a database that changed, not an attacker.

## Tests

| Test | What it pins |
|---|---|
| The tier matrix (`typed_scripts_agree_however_they_compile`) gains a mode that wraps each generated script's definitions in a package, installs it and calls it | file code prints what every other tiering prints |
| The same mode with every lookup made to fail at attach (`RY_AHEAD_MISS=1`) | a rejected version falls back to a session compile with the same output |
| `install-typed-package.scenario` gains the attach log line, a second process, a stale file (the `.rdb` touched) and a file from another build | the user journey and each refusal |
| `fuzz_typed_code_file`: mutated file bytes through `parse` and attach | never a panic, and never an attached function, because a hash or a bound fails |
| `fuzz_typed_code_parse`: mutated metadata with the hashes recomputed, through `parse` only | never a panic and never a read outside the file |
| A written file read back | every column round-trips, and two writes give the same bytes |
| The typed tests under `RY_GC_STRESS` with attached code | sites and constants are rooted as session code's are |
| `robustness.md` gets a row for the code file with these harnesses | the contract stays current |

## Benchmarks

The suites time scripts, and a script has no installed code. `tools/bench/bench.py` gains an
engine `ry_typed_file` beside `ry_typed` and `ry_typed_aot`: it generates a package from each
typed program's top-level definitions, installs it once outside the clock, and runs a driver that
loads the package and calls the measured function. It reports the first run and the warm time,
which `docs/src/lib/benchmarks.ts` renders as a "typed, installed" column and a row of the
first-run table. The published numbers need a `just bench <suite> --release ARCHIVE --write` run
on a release that has the file.

## The executable

`ry build --aot --exe main.R -o app` comes after the package file and reuses its format. The
output is a copy of the running `ry` executable with a payload appended: the elaborated script,
the strict files it sources, a code file for the script's top-level definitions, and a trailer
with the payload's offset and length. A script's closure is named by the index of its top-level
expression and its position in it, in place of the record offset. At start, `ry` looks for the
trailer on its own executable, and if it finds one it runs the script as `ry run --aot` does,
attaching from the payload instead of compiling. Packages load from the library. On macOS the
appended file must be signed again (an ad hoc signature is enough), as Bun does for
`bun build --compile`.

## Untyped code

Untyped R gets no machine code in the file (`decisions.md`, "Untyped R code is not compiled to
machine code ahead of time"): on the applications suite tier 1 buys nothing to move earlier, and
on the kernels its compile is 2 to 9 ms per program, about 9% of a cold run. A trained bytecode
image already carries the run's profile, so tier 1 compiles a hot body at its first calls. The
file format leaves room for tier 1's machine code as more versions with guards, and that becomes
worth building when the applications suite shows tier 1 winning and its compile showing in a cold
run.

## Open questions

- **Executable file mappings on macOS and Windows.** Whether macOS on Apple silicon runs
  unsigned file pages mapped executable, and how Windows maps them, is unverified. The copy path
  is correct on both.
- **A `match` over any tag.** Its code tests for the shapes confirmed when it compiled. Keeping
  such a function in the file needs a rule for a process that has confirmed more shapes: store
  the set the code tests and attach only when the process's set is equal, or dispatch on the
  shape through a table the loader extends. The first version leaves these functions out and
  counts them in the install log, so the choice can be made on that count.
- **Direct calls as facts.** A sibling call inside a locked namespace is checked on every entry
  from R, as a guess. Making it a fact, dropped when a binding is unlocked, is the typed
  runtime's "Redefinition" design and is independent of the file.
- **A binary package.** `ry build --aot` installs. Whether it should also write a binary tarball
  that carries the file to another machine with the same ry build is open.
