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

An earlier prototype of install-time typed code, which is not in the tree, measured 50 typed
functions of a generated package on an iterate build: their first calls took about 1 ms from the
installed code against about 40 ms compiled in the session, and added 1.8 MiB of peak resident
memory against 5.9 MiB. `ry run --aot` on the typed benchmark
programs spends 1.1 to 5.9 ms per program compiling up front (50.6 ms for `pidigits`), which is
the cost a code file takes out of a process.

| Measure | Target | Why this number |
|---|---|---|
| Attaching one function at its first call | at most 20 µs | the prototype's 1 ms over 50 first calls |
| Compile work in a process whose functions are all in the file | zero `typed: compiled for` lines | the directive: nothing compiles mid-run |
| Warm time of file code over code compiled in the session | within 2% (geometric mean, typed suites) | same compiler, same IR: only address loads differ |
| `library(pkg)` with a code file over without one | at most 0.1 ms more | the file is opened at the first typed call, not at load |
| Proportional set size of the code pages with four processes | about a quarter of the resident size | the pages are shared, only the table is private |
| Typed benchmark functions that are in the file | every function `ry run --aot` compiles up front for the same program | the file must not cover less than the script path does |

## What the user sees

Nothing on this page is built. The transcript shows the designed output.

```console
$ ry install pkg -l library
$ RY_TYPED_LOG=1 ry run use.R        # use.R calls library(typedpkg); total(1000, 0.001)
typed: attached `total` for [I64, Dbl] -> Dbl from typedpkg's code file
[1] 718640.9
```

`ry install` writes the code file whenever the package has typed functions, and `ry build --aot`
on a package directory does what `ry install` does (it accepts `-l`). `--ry-no-typed-code` on the
underlying `R CMD INSTALL` skips the file, as `--ry-no-code-image` skips the bytecode image. A
package without the file, or with a file from another ry build, compiles its typed functions at
their first call. The design adds one case to that: under `ry run --aot` such a package's typed
functions compile when the namespace loads, with one line that names the package and says to
reinstall it.

The file covers what the checker typed, so `ry install` first runs the preflight over the package
and installs the elaborated sources (work list, step 1). Without that step an installed function
that converts an R value has no checked conversion type and waits for an interpreted call, and a
parameter annotated with a named type (`alias`, `type`) cannot be compiled for its declared type,
because an installed namespace carries no declarations to follow the name through.

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
  instead of stopping (work list, step 3).
- **Nothing else.** A function the lowering refuses is not in the file and runs in the
  interpreter. A function with a `match` over any tag is left out of the first version, because
  its code tests for the shapes the installing process had confirmed, and a process with other
  typed packages loaded has confirmed more ("Open questions"). A function whose code needs a
  relocation the writer has no slot for is left out too, with a line in the install log, so the
  file never holds an unresolved relocation.

The file's versions belong to the closures the package database holds. A copy of such a closure
made at run time (an R6 method bound by `$new()`, a function after `environment<-`, the per-class
copy of an S4 method) is another closure with another environment, and it compiles in the
session.

## Position-independent code

The backend embeds process addresses as 64-bit immediates today: every runtime helper call is
`iconst(address)` followed by `call_indirect` (`call` and `call_words` in
`crates/typed/src/backend.rs`, 102 uses of the `address!` macro), and so are typed callees
(`call_typed`), R call sites (`call_r`), the free stubs (`preserving`, `count`), static strings
and static boxes (`Constant::Str`, `Constant::Object`) and the interrupt flag (`Op::Poll`). Shape
ids are process values too, and they enter the code in three places: `construct` in the backend,
and two places in `crates/typed/src/lower.rs` that turn an id into a plain integer constant
(`self.int(shape as i32)` in the cases of a `match`, and the `GT` tag the sort comparator tests
for). None of those values is the same in another process.

File code loads each such word from a table instead. Cranelift already emits that load: with
`is_pic` set, `symbol_value` of an external name that is not colocated compiles to
`mov name@GOTPCREL(%rip), %reg` on x86-64 (relocation `X86GOTPCRel4`) and to `adrp` plus `ldr` on
AArch64 (`Aarch64AdrGotPage21`, `Aarch64Ld64GotLo12Nc`). The table is that global offset table.
Each word is one external name, numbered by its slot, and the writer resolves each relocation to
the slot's distance from the instruction, which is fixed because the table is mapped at a fixed
distance before the code. A slot is 8 bytes, and the table and the code share one reservation
far smaller than the 2 GiB a 32-bit displacement reaches. A call between two functions of the
file is a direct relative call (`X86CallPCRel4`, `Arm64Call`). No relocation is left in the file,
so its code pages are never written and every process shares them.

Two more relocation sources exist on a PIC ISA, and the writer handles both. Every external name
goes through the offset table there, colocated or not (`load_ext_name` in Cranelift's x64
`emit.rs` and AArch64 `inst.isle`), so the file's own read-only data cannot be addressed relative
to the instruction: it gets a slot. And `crates/ssa` builds its ISA with
`use_colocated_libcalls` off, so a libcall (`fmod`, a rounding function on a CPU without the
instruction) is an offset-table load against `ExternalName::LibCall`: the writer maps each
libcall to a `Helper` slot.

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
| `Data(offset)` | the address of the mapped read-only data plus `offset` | when the file is mapped |
| `Site(index)` | a `*const RCall` built for R call site `index` | when its function attaches |
| `Callee(index)` | the entry address of a typed function in another namespace | when its function attaches |

Helper ids are positions in one static array of the helpers' addresses, which replaces the
`address!` uses, so a helper is named once. The array also lists the libcalls the backend can
emit. The build stamp makes the numbering safe: a file is read only by the ry build that wrote
it. A static string lives in the file's read-only data in the layout `ry_str_static` gives one
in a session, the 8-byte buffer header with the static count followed by the bytes, and its
`Data` slot holds its address. That is sound because a mapped file is never unmapped (see
"Loading"). Rewriting the load of a `Data` slot into an address computation (`lea` on x86-64,
`adrp` plus `add` on AArch64, the relaxation a linker applies) would save the load; it is an
optimization to take only if the warm-time target is missed.

Session code keeps its immediates. Both targets go through one seam in the backend, an
`Address` that is either an absolute word or a slot, so the difference is confined to `call`,
`call_words`, `call_typed`, `call_r`, `preserving`, `count`, the constant arms, `construct`
and `Op::Poll`. Lowering becomes target-independent: `Constant::Str` carries the string, not a
pointer from `ry_str_static`, `Constant::Object` carries the shape, not a box address, and a new
`Constant::Shape` carries a shape wherever lowering writes its id as an integer today. No
`Constant::Int` may hold a shape id afterwards. The test that attaches a file in a process which
interned other shapes first pins that ("Tests").

`crates/ssa` gains what the file target needs. Its lowering of existing instructions keeps its
output. The tier-1 track owns the crate and has agreed to both additions on three conditions:
`ssa::Relocatable` does not depend on frame states (`FrameState` and `emit_exits` in `ssa.rs`
serve tier 1's exits, and a typed body has none), `table_word` sits beside the other
instructions that produce an address, and the JIT module refuses it with an `Error`, not a
panic.

- an IR instruction `table_word(slot)` that lowers to the `symbol_value` above, and a direct call
  to a function of the same object;
- `ssa::Relocatable`, a module kind beside the JIT module. The lowering in `crates/ssa/src/lower.rs`
  is generic over `cranelift_module::Module`, so `ssa::Relocatable` either implements that trait or
  the lowering takes a narrower trait of the three operations it uses (declare a function,
  declare an external name, define); the second is less code and is the plan. `ssa::Relocatable`
  compiles each function with `cranelift_codegen::Context::compile` on an ISA built with
  `is_pic`, keeps the bytes and the relocations (`MachBufferFinalized::relocs`), lays the
  functions out and resolves every relocation: the offset-table loads (`X86GOTPCRel4`,
  `Aarch64AdrGotPage21`, `Aarch64Ld64GotLo12Nc`) against table slots, including those for
  libcalls, and the calls (`X86CallPCRel4`, `Arm64Call`) against functions of the object. Any
  other relocation kind refuses the function. It is about 300 lines and needs no new dependency.

The writer does everything that reads R objects or the typed core's thread-local state on the
main thread: lowering, the IR passes, and the emission of Cranelift IR, which reads the shape
table and the free stubs (`SHAPES` in `shapes.rs`, `FREES` in `backend.rs`). Only
`Context::compile`, which turns one function's Cranelift IR into bytes, runs on worker threads.
The results are laid out in the order of the `functions` column, so the file's bytes do not
depend on thread timing.

## File format

The file is `R/<package>.<build>.ryt`, beside the bytecode image `R/<package>.<build>.ryc`
(`rcmd::images`). All integers are little-endian, every offset is a `u32` from the start of the
file, and every table is a struct of arrays: one array per field, all of the same length.

| Part | Contents |
|---|---|
| Header | magic `RYTYPED\0`, format version, the build identity, the length and modification time of the package's `.rdb` (the bytecode image's `DatabaseStamp`), a hash of the target triple and ISA flags, the section directory, a hash of every section before the code, and a hash of the code part's shared bytes (free stubs and read-only data) |
| `functions` | per compiled version, sorted by (record, position, signature): the `.rdb` offset of the closure's lazy-load record, the closure's position in that record, its signature, the code offsets of its body and its trampoline, its code length and code hash, and its ranges in `lookups`, `sites`, `callees` and `confirms` |
| `signatures` | parameter types and result type of each version, in the encoding of `ir::Type` with every process-local id (`Type::Record`, `Type::Tag`, `Item::Array`) replaced by an index into `shapes` |
| `lookups` | per name a version resolved when it compiled (a `Speculation`): the symbol, where the lookup starts, and what it must find (the kinds are listed below) |
| `sites` | per R call site, every field of its `RCall`: the closure whose body holds the call (record, position) and the path to the call expression as child indices, the lookup of the called function, per argument its type and its conversion (the conversion's lookup and its checked item type), how the result is used (`RResult`), and the flags `diverges` and `effect_free` |
| `callees` | per call of a typed function in another namespace: its lookup and the signature the caller compiled against |
| `confirms` | per version, the shapes its code constructs, as indices into `shapes` |
| `slots` | per table word: its kind and payload, as in the table above |
| `shapes` | per record layout, tag union and array type the code or a signature names: its field names and item types, enough to intern it in another process |
| `strings` | the bytes of symbols and field names |
| Code | starts at a multiple of 64 KiB, after zero padding: function bodies, trampolines, the free stubs and the read-only data |

A lookup records how lowering classified what the name resolved to, because lowering specializes
on more than a function's identity, and attaching must find the same class:

| Lookup kind | What lowering did with it | What attaching requires |
|---|---|---|
| A function of this file | a direct call, or the function inlined | the closure with that (record, position) |
| A base function | code for the builtin by name (`is_base_function`) | the same test passes for what the name finds |
| A table function | a call that gives a table value (`is_table_function`) | the same test passes |
| A standard library constant | its value inlined (`library_constant`) | the name finds the standard library's own binding |
| A typed function of another namespace | a call through a `Callee` slot | a typed closure that compiles or attaches for the recorded signature |
| Any other R function | a call through a `Site` slot | a function, which goes into the site's `RCall` and the entry's speculation |

A site inside an inlined sibling sits in the sibling's body, which is why a site names its own
closure and not the version's.

A closure is named by the offset of its lazy-load record in the `.rdb` and its position among the
closures of that record, in the order `serialize::unserialize_with_closures` yields them. The
`.rdb` stamp in the header ties those offsets to one database. The bytecode image names closures
the same way. Only records of the package's own `R/<package>.rdb` are named; `sysdata.rdb` has
its own offsets and its closures are not in the file.

## Writing

The install driver that writes the bytecode image (`crates/rcmd/src/code-image.R`, run by
`write_code_image` in `crates/rcmd/src/rcmd.rs`) loads the namespace, runs the training workload
if one was asked for, and writes the image. The code file is written by the same session, after
the image, through an internal function `ryTypedCodeWrite(package)` beside `ryCodeImageWrite`.
That session does not run when `--ry-no-code-image` is given, so the options become independent:
the driver runs when either output is wanted and skips the one that is not. `images::prune`
(`crates/rcmd/src/images.rs`) removes the `.ryt` files of other builds as it removes their
`.ryc` files.

1. Force every binding of the namespace through the loader's note path ("Loading"), which
   records each closure's record offset and position.
2. Run the up-front pass over the namespace's closures (`typed::ahead::compile`, with the
   bindings as its definitions).
3. For every successful entry of `COMPILED` whose closure belongs to the namespace, lower it again
   for the file target and collect its lookups, sites and callees.
4. Compile on worker threads, lay out, resolve relocations, and write the file to a temporary
   name in the same directory, then rename it into place.

Writing the file twice for one installed package (the same `.rdb`, the same ry build) gives the
same bytes. A test pins that. Two installs differ in the header's `.rdb` modification time and
in nothing else.

## Loading

**Finding a closure's code.** `do_lazy_load_fetch` (`crates/interp/src/serialize.rs`) knows the
record offset it reads, and `unserialize_with_closures` knows each closure's position. For a
namespace whose package has a code file, the fetch notes each typed closure's (record, position)
and the database's stamp as it was when the database was first read. The note is keyed by the
closure object itself, as `COMPILED` is, and lives as long as the closure (the lifetime rule of
`note_pending` in `crates/interp/src/code_image.rs`, which keys on the body; the body is the
wrong key here, because copies of a closure share it and `take_pending` replaces it). A fetch
reads a record from one of three sources, and the plain database source calls `unserialize`,
which yields no positions, so that source goes through `unserialize_with_closures` when the
package has a code file. A package without the file notes nothing, so untyped packages pay
nothing.

**Opening.** The first miss in `compiled_or_refusal` for a closure with a note opens the file:
it reads the header, checks the magic, version, build identity and target hash, checks the
`.rdb` stamp against the stamp in the note (not against the database on disk, which a reinstall
in a running session has replaced while the notes still name the old one), bounds-checks every
section and index, checks that the padding before the code is zero, and checks the two header
hashes. Any failure marks the
namespace as having no file, with one `RY_TYPED_LOG` line that says why. Parsing is a pure
function over bytes (`parse(bytes, build, stamp, target)`), so it is fuzzed without a session.

**Mapping.** The loader reserves one address range for the table and the code, maps the file's
code part over the upper part as read-only and executable, and leaves the table as private
writable memory below it. It interns every entry of `shapes` and fills the `Helper`,
`InterruptFlag`, `Shape`, `Constant` and `Data` slots. A mapping lasts for the life of the
process: static strings point into it, and frames of its code may be on the stack when a
namespace unloads. A reinstall renames a new file into place and leaves the mapped one intact.
Where the system refuses an executable mapping of file pages, the loader reserves the same range
as anonymous memory, copies the code part into the upper part, and then makes that part
read-only and executable (`MAP_JIT` with write protection toggled on macOS). The table keeps its
fixed distance, which the JIT's own code memory could not give. The copy keeps the zero compile
and gives up the sharing, and it is the path for every platform until the mapping is verified
there.

**Attaching.** A function is attached when it is first asked for, and attaching builds the same
`Compiled` entry a session compile would:

1. Find the version by the closure's note and the parameter types. No note or no version is a
   miss: the function compiles in the session. A version already attached to another closure is
   a miss too, which cannot happen while notes are per closure and is checked all the same,
   because a version's `Site` slots hold one closure's environment.
2. Check the code hash of the version.
3. Resolve each lookup in the live session, as `still_resolves` does for session code, and
   require the class the lookup recorded (the table under "File format"). A lookup that finds
   anything else rejects the version.
4. Build the `RCall` of each site from its recorded fields, with the environment of the closure
   being attached, and fill its slot. Resolve each callee in another namespace through
   `compiled_or_refusal`, which attaches or compiles it, check its result type against the
   signature the caller compiled against, and fill its slot.
5. Attach every function of this file the version calls directly, depth first, with a state per
   version (unattached, attaching, attached, rejected) so that recursion ends. A rejected callee
   rejects the caller, because the caller's direct call would run code that is not valid.
6. Confirm the shapes in the version's `confirms` range through `shapes::confirm`, the function
   a session compile uses, so that code compiled earlier in the session with a `match` over any
   tag is treated as it is after a session compile.
7. Insert the entry into `COMPILED` and root the closure (`add_global_root`), as
   `compiled_or_refusal` does. The entry's speculations are its own resolved lookups and those
   of every callee, direct or in another namespace, and its epoch is the earliest of theirs,
   which is what `Linker` computes for session code. `calls_r` is set when the version or a
   callee has a site.

An attached entry holds no JIT memory and the installed files bound how many there are, so it
does not count toward `MAX_COMPILED`, as a function compiled ahead from a script's text does
not.

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
| `fuzz_typed_code_file`: mutated file bytes through `parse` and attach | never a panic, and never an attached function: every byte is under a header hash, a per-function code hash or the zero-padding check |
| `fuzz_typed_code_parse`: mutated metadata with the hashes recomputed, through `parse` only | never a panic and never a read outside the file |
| A written file read back | every column round-trips, and two writes for one installed package give the same bytes |
| A package whose function is copied at run time (`environment<-`, an R6 method), calls R, and is called through both closures | the copy compiles in the session and each closure's R calls run in its own environment |
| `install.packages` of a changed package in a running session, then a first call of a function fetched before it | the stale file is refused by the note's stamp |
| A package with `match` over a closed union, a sort with a comparator and a record parameter, attached in a process that interned other shapes first | shape ids come from the table, never from the installing process |
| The typed tests under `RY_GC_STRESS` with attached code | sites and constants are rooted as session code's are |
| `robustness.md` gets a row for the code file with these harnesses | the contract stays current |

## Benchmarks

The suites time scripts, and a script has no installed code. `tools/bench/bench.py` gains an
engine `ry_typed_file` beside `ry_typed` and `ry_typed_aot`: it generates a package from each
typed program's top-level definitions, installs it once outside the clock, and runs a driver that
loads the package and calls the measured function. It reports the first run, the warm time and
the number of functions that compiled in the session (`typed: compiled for` lines), so a program
the file does not cover shows as that and not as a fast first run. It
feeds a "typed, installed" column and a row of the first-run table in
`docs/src/lib/benchmarks.ts`. The published numbers need a `just bench <suite> --release ARCHIVE --write` run
on a release that has the file.

## The executable

`ry build --aot --exe main.R -o app` comes after the package file and reuses its format. `-o`
names the tarball's directory for a package build, and with `--exe` it names the output file
(default: the script's name without `.R`, in the current directory). The
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
