# Decisions index

Settled architecture decisions and the reasons behind them, one file per area. Every entry carries a `**Status:**` line under its heading:

- **built**: the code does what the entry says.
- **planned**: the design is settled and not fully implemented. The line says what exists so far.
- **superseded**: another entry replaced this one. The line names it and says what the text is kept for. A superseded entry with nothing left to teach is deleted.

An entry listed here without a status is built. `check` fails on an entry without a status line. To read one, search its heading in the area file. A new entry gets its status line when it is written, and the line changes in the same change that builds or replaces the design. A new entry is an H2 section appended to the area file it belongs to; then run `python3 tools/memory.py index` to regenerate this file, which `python3 tools/memory.py check` (part of `just gate`) keeps honest.

## Syntax and semantics

[`syntax-semantics.md`](syntax-semantics.md): The parser and syntax tree, the salsa analysis core, name resolution, and the type checker.

- The standard library ships as declaration-only stub files
- The beta semantics
- Beta semantics: The R variable model is mutable slots with union joins
- Beta semantics: A multi-member union is for joins and annotations, never for a unification variable
- Beta semantics: A type error inside a test assertion stays reported (built, with exceptions)
- Beta semantics: The type system is Hindley-Milner, and stays fast to check
- Beta semantics: ry has no macros
- Beta semantics: An overload set is a bounded, ordered probe
- Beta semantics: The boundary for R's object systems is dispatch, not Hindley-Milner (built, with exceptions)
- Beta semantics: `T[]` carries an atomic-element constraint, not a trait
- Beta semantics: Coercion policy at parameter positions
- Beta semantics: Strict mode
- Beta semantics: Guard narrowing (built, with exceptions)
- Beta semantics: The expanded parameter directive is `@param name {TYPE}`
- A syntax error does not erase a file
- `is.null` shapes an unconstrained inference variable (built, with exceptions)
- Data-masked resolution for data.table and the with-family (built, with exceptions)
- Recursion in a function-valued assignment is monomorphic (built, with exceptions)
- Bare-name resolution is not gated on NAMESPACE imports
- The shipping stack is a hand-written parser, a lossless tree, and salsa
- Shipping stack: Why the previous stack was replaced rather than tuned
- Shipping stack: Why a lossless tree, and why ry's own
- Shipping stack: Why salsa rather than an in-house engine
- Shipping stack: Item granularity and item identity (planned)
- Shipping stack: An annotation is one concept with pluggable spellings
- Shipping stack: The inline annotation syntax is a superset of R (built, with exceptions)
- Shipping stack: How inline annotations are built
- Shipping stack: Inline type definitions, coercions and nominal constructors
- Shipping stack: Testing doctrine for `syntax`
- Shipping stack: Cases where R parsers get subtle
- Shipping stack: Corpus mechanics (built, with exceptions)
- Shipping stack: Better syntax errors are a goal, not a side effect
- Deep resolve cuts a cycle to `Unknown` and resolves a shared subtree once
- The interface fixpoint is canonical per group, so a cyclic scheme does not depend on forcing order (built, with exceptions)
- There is no third constraint kind, so two flexible comparison operands stay unconstrained (built, with exceptions)
- Union compatibility commits a flexible argument at first use, in program order
- Strict mode attributes a recursive binding the fixpoint cannot fully type
- Script frame semantics (built, with exceptions)
- An undeclared type name errors once at the reference and compares like `Unknown`
- An annotation shape violation refuses the whole block
- A statement-level annotation attaches at any depth and applies where the expression infers
- Capture liveness is frame-scoped
- Conditional slots type, export edges generalize, and `missing()` flows through the environment
- NAMESPACE and DESCRIPTION metadata feed resolution
- Data.table awareness (built, with exceptions)
- The native pipe desugars at lowering
- `@masked` is formal-aware, and dplyr is a conditional namespace
- A vendored export manifest carries the name-level truth beside the typed stubs
- One source of truth for where an item spells its name
- A check reads per-name firewalls, never project-wide maps (built, with exceptions)
- Cold-pass memory and time: interned names, compact HIR, parallel setup (built, with exceptions)
- Syntax trees are evicted, and only small syntax facts are memoized (built, with exceptions)
- A call's result drops unconstrained type-parameter members of a union
- The cold check searches green trees and caches globals per item check
- Call checking follows R's lazy argument and matching rules (built, with exceptions)
- Called names are looked up as functions
- testthat files mirror testthat's environments
- useDynLib native symbols resolve only as a native routine's name
- Comparisons coerce across atomic types
- A test-guarded write to a formal normalizes it; inferred types admit the default
- An open `T | NULL` parameter binds `T` to the argument's non-`NULL` part
- `switch(typeof(x), ...)` narrows `x` per arm
- Nested replacement rewrites the spine
- A strict library file's top level holds only definitions, and purity is declared
- Cold-check constant costs: one green walk per file, immortal interning, high durability (built, with exceptions)
- Types and names are hash-consed outside salsa
- The checker presumes R code is correct where R's semantics leave the question open
- A computed function that may be `NULL` is called as the function
- A file's check names only what its interface reaches (built, with exceptions)
- Every host keeps every tree
- `ry check` releases its trees once, and memoized records are compact (built, with exceptions)
- HIR as struct-of-arrays, lowered from the green tree (built, with exceptions)
- A function literal in a call argument is typed in context
- Shipped stubs are whatever `types/` holds
- Override-stub problems are computed after `PackageMetadata` exists
- Strict mode implies type errors
- An all-`Any` overload does not veto agreement among unchecked fits
- A formula is the nominal `formula`, and its operands are not judged
- `$`, `[` and `[[` dispatch to a class's subset method
- A new top-level name costs one project fold (built, with exceptions)
- Typed checking costs per name, not per file
- A conditional write of a formal covers the paths that run it
- `<<-` writes flow forward, and a NULL-initialized cache reads its own first write
- The overload probe's unchecked-argument witness is a nominal no candidate declares
- A call on a parameter whose arity the body tests is unchecked, and a later sibling does not force
- Corpus-driven stub and overload fixes
- Strict-friction rules for matrices, unions, `...: Any` and `$` members
- Inferred annotations follow the project's annotation style
- The cold check's per-call work: a bump arena, borrowed schemes, a token-only green builder (built, with exceptions)
- The item check's working maps live in a pooled bump arena (built, with exceptions)
- `$` fields resolve through the values that built the receiver
- Packages without stubs are read from the R library by the host, lazily
- One-shot checks record coarse dependencies (superseded by "Every host resolves names through one path, and trees live in the host's store")
- Inferred signatures carry body facts that a call reads
- `Reduce` folds a named function instead of matching `fn(U, T) -> U`
- References are checked against an independent resolver, and a replacement copy is its source
- `if` in NAMESPACE is evaluated by the host, then resolved in text
- Reference Class and R6 method bodies resolve object members in naming
- A literal `source()` path is followed for resolution
- A masked argument is its own scope, and the base masking family is typed
- `pkg::name` on an inactive shipped namespace checks the shipped export list
- A name inside a string's `{...}` is a read for liveness
- An `if` with an `Any` branch is `Any`; a two-path write still unifies
- tidyr and tibble ship masked-verb stubs beside their manifests
- `length(x)` guards drop `NULL`
- A numeric binding yields to a matrix or a generic argument
- A splice reparse refuses a prefix with an unclosed delimiter
- Alias cycles are reported at the declaration, and short names get narrow suggestions
- The syntax tree is ry's own flat preorder arena
- Overload probes reject by call shape first
- `[[` by name on a named atomic vector is the element, not `T | NULL`
- Correlated-path allowances added for the fresh sample
- A cold check holds one parse tree per worker (built, with exceptions)
- The one-shot check scans kind columns and overlaps its serial phases (built, with exceptions)
- The checker's per-item facts are columns and bitsets indexed by dense id
- Every host resolves names through one path, and trees live in the host's store
- Types and symbols are four-byte ids into append-only columns (built, with exceptions)
- An item's syntax has a stamp, so an edit lowers only the items it changed
- The analysis core's per-item and per-file tables after the type store
- A function has one signature, resolved in `syntax::annotation`

## Editor and language server

[`ide-server.md`](ide-server.md): The language server, editor features, the formatter, lints, the check command's reporting, and the console and debugger.

- Beta semantics: A reported column counts characters, and caret art counts terminal cells
- Beta semantics: The CLI reports through miette
- Beta semantics: The annotation formatter parses, then pretty-prints
- Beta semantics: Config subsystem (built, with exceptions)
- The Zed extension versions on its own line (superseded by "Releases are built natively per platform, with the runtime beside the binary")
- Editor features for ry's own syntax
- Editor responsiveness at 300,000 lines
- A parallel startup warm-up in the server
- A fix says whether it may be applied unseen
- Parameter hints and call hierarchy follow R's own call rules
- The debugger lives inside the interpreter (built, with exceptions)
- Range formatting is a cut of the whole-file format
- Bug lints are on by default, style lints are opt-in
- `ry check` keeps an on-disk record of its findings
- The data-table lint is opt-in and marks only constructors
- The language server reads the check record
- Literate documents keep their offsets, and the server keeps their original text
- A running console is a debug adapter, a data source, and a live-value source on one socket
- A parameter and its named arguments are one editor binding
- R's dynamic help server runs on the interpreter thread, only while R waits
- Completion, signature help and import fixes reach untyped and installed code (built, with exceptions)
- Any save refreshes every open document's diagnostics
- A call-hierarchy item answers only in its own document
- The console test waits on a count of handled closes
- Refactorings refuse rather than guess, and a seeded property check holds them to it
- Call-head completion ranks by call counts, without test files
- Call hierarchy and workspace symbols are measured against the reference oracle
- Completion pushes down names whose declared type cannot fit, and never promotes
- The console's crossterm is patched to drain the terminal, and R reads console input one line per chunk

## Interpreter

[`interp.md`](interp.md): The evaluator: calls, promises, frames, environments, lookups, dispatch and conditions.

- The interpreter and base R (built, with exceptions)
- `[`, `[[`, `[<-` and `[[<-` dispatch before evaluating their other arguments
- `object-systems = false` stops class definitions in project code at run time
- Smaller runtime choices
- The interpreter's call path, lookups and allocation (built, with exceptions)
- A primitive's S4 cache lookup matches arguments by name first
- A hashed frame counts complex assignment's `*tmp*`
- Single-thread first, parallel-ready architecture (user directive) (built, with exceptions)
- Argument-match caches live in the callee's code, not at the call site
- Cheap promises: measured, not redesigned; call setup and package loads trimmed instead
- Promises per call: native base closures take arguments by value; the rest of the promises are observable
- Slot frames: closure locals live in registers until something asks for the environment
- A slot frame's environment binds a variable only when a lookup asks for it
- A slot frame's environment asks its frame only for names the frame binds
- Single-core speed first; per-process costs count once per worker (built, with exceptions)
- Closure call setup: the 10x end shape (planned)
- Tier-0 lookups through binding cells
- Native entries read promises over a pending frame without forcing them
- Interpreter hot paths make no Rust allocations
- Closure frames live in fixed slots that are never moved
- Arguments that name a local stay lazy
- The fixed cost of an argument reference
- S3 methods are found through one row per method symbol
- The interpreter is an engine crate between a numeric crate and four leaf crates
- A watchdog thread raises the poll flag at a time limit's deadline, and the interrupt check raises the error
- Measured and not taken: O(1) `Sys.getenv` and `options`
- Measured and not taken: a leaner `format()` on the validation workload
- Base fast paths read formals one at a time, except `formatC()`
- Parked: cheaper restart setup in `withRestarts` (planned)

## Builtins and base R

[`builtins.md`](builtins.md): Base R's builtins and native kernels, numerics, regular expressions, connections, the default packages, and package code that runs natively.

- One connection table, in the interpreter
- Base R's linear algebra
- The default packages' utils, tools, grDevices and graphics routines
- parallel's fork routines
- One Rmath, one codec stack, and the numerical routines
- POSIX regular expressions run on an automaton when they need the longest match
- `perl = TRUE` runs on PCRE2, linked statically
- TRE approximate regular expressions
- TRE minimal repetitions over alternatives
- The message sink and connection flush points
- utils' sockets, internal downloads and allocation profiling
- One session temporary directory, removed at exit
- Base R kernels: radix sorting, gather subsetting, x87 accumulation and shortest-digit formatting (built, with exceptions)
- Base R kernels: position tables, a most significant digit radix order, and a per-call string memo
- Base kernels pick their data structure by the value range
- `as.character` of a long number vector is deferred
- `format()` of numbers streams into the result
- The methods package's core runs natively behind hash-guarded prologues (built, with exceptions)
- Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate
- Hot base R closures get hash-guarded native prologues
- Builtin fast paths take names as CHARSXPs and evaluate nothing twice (built, with exceptions)
- One-argument builtins and small base closures skip dispatch when their arguments are values
- Builtins with positional direct entries, one implementation per semantic (built, with exceptions)
- Builtin call machinery and subassignment toward 10x
- One kernel layer for R's vector builtins: slice kernels, a reuse rule, and a fused superinstruction (built, with exceptions)
- Package code that dominates a workload runs natively: rlang's trace_back
- withr's environment and option helpers run natively; defer() stays in R
- Environment variable calls read `environ` once
- Conditions: message(), warning(), signalCondition() and getOption() run natively when nothing could observe the difference
- Builtin and assignment hotspots of the public suites
- One-pass attribute lookup and the blocked matrix product
- Distribution functions and the RNG run as vector kernels and batched samplers
- unlink() follows GNU R's R_unlink, with directory descriptors
- Parse data as dense columns
- Stats distribution closures enter their routine directly
- Base `.Internal` wrappers inlined; string fast paths not kept (built, with exceptions)
- The serialize reader checks what a stream claims before it stores it
- sprintf formats numbers with the native kernels
- Hash tables for `unique()` and `match()` stop growing with the elements at 256 KB
- Positional builtin calls read their arguments from the registers (built, with exceptions)
- A warning reaches `suppressWarnings` and `tryCatch` without running R
- `%%` and `%/%` on doubles port GNU R's kernels, with a 64-bit path for whole numbers
- `validObject` runs validity methods without its slot loop, and `new()` copies the prototype shallowly
- Small base closures with call arguments run their bodies natively behind prologues
- `c`, `unlist`, `rep` and positional `[` copy handle vectors a run at a time
- `x[] <- value` and `x[a:b] <- value` write whole copies of the value

## R compatibility

[`r-compat.md`](r-compat.md): Matching GNU R's observable behavior: the oracle, corpus package tests, and R semantics details.

- Package tests through the oracle
- Corpus package tests: compatibility details that are easy to get wrong
- Matching GNU R's own tests where its behavior is an artifact (built, with exceptions)
- Runtime semantics settled by the corpus package tests
- Package-test compatibility details that look optional but are not
- Runtime error checks follow GNU R's order and blame
- A formula's dot expands to the sum GNU R prints
- The corpus's waiting is the suites' own sleeps, and child R processes run ry
- The package corpus runs offline
- The oracle suite names the GNU R build it was recorded with
- The oracle gives every engine the reference R's library paths
- Known oracle failures live in a committed file
- A crash is never a known failure
- The oracle refuses a partial run and finds the corpus library itself

## JIT and SSA

[`jit-ssa.md`](jit-ssa.md): Tier 0, the Cranelift tier, the shared SSA IR, deoptimization, and what compiles when.

- The JIT tier (built, with exceptions)
- Native code sees a compiled closure's body as its expression
- Native scalar functions: frameless calls with checks proven at compile time (built, with exceptions)
- Compiled function lookups also check the activation's own frame
- An integer and a double joined stay unboxed
- The JIT leaves a lossy double modulus to tier 0
- Compiled bodies share one deoptimization exit
- Native functions loop, read vectors, and inline small callees
- Closures made at one site share compiled code
- Fast-path prologues return through an op, not an unwind
- Bodies that would mostly hand back to tier 0 compile only for loops
- Tier 0 records type feedback only for warm bodies (superseded by "Tier 0 records feedback only for bodies halfway to a threshold")
- A typed closure compiles at its first call only when that call can run long
- Strict-proven types do not drop JIT guards yet
- Untyped R code is not compiled to machine code ahead of time
- Bytecode is build output, not a cache (built, with exceptions)
- Tier 0 runs on facts fixed before the code runs (built, with exceptions)
- The untyped performance roadmap is ordered by measured gap, and the baseline tier is Sparkplug-style (built, with exceptions)
- Step 2 of the untyped roadmap: slot frames rejected on measurement, setters compiled, step 3 pulled forward (built, with exceptions)
- One SSA IR and one install-time pipeline for typed and untyped compiled code (built, with exceptions)
- The effect-free fact is stored per code object and resolved per enclosure
- Why the JIT made code slower, and the three fixes (built, with exceptions)
- Tier 1 probes the frame inline for a variable load
- Bodies wait in proportion to their size before tier 1 compiles them
- Tier 0 records feedback only for bodies halfway to a threshold (built, with exceptions)
- Bodies that could run nothing natively never record
- A loop-free hot body compiles only when its call rate repays the compile (built, with exceptions)
- Tier 1 keeps the 3000-entry threshold
- Tier 1 inlines block-bodied closures, and a failing inline reruns the whole call in tier 0
- The native execution model: one design for frames, calls, unboxing and install-time code (planned)
- Tier 0 decides at scheduled entries and stops recording at tier-up
- Tier-0 element reads and stores as fast handlers, not new ops
- Computed subscripts stay in bytecode
- Compiled code stores list elements and kind-checked vector elements itself
- The road to 3x (planned)
- Tier 0 runs frameless fast handlers, quickened arithmetic and out-of-line loop fallbacks (built, with exceptions)
- Tier 1 compile throughput: linear SSA construction and one compile per body (built, with exceptions)
- A compiled element op costs no more than tier 0's handler
- Tier 0 compiles whole bodies
- Tier-0 call entry: raw pool reads, an out-of-line tier schedule, and a cleared register window
- Tier-0 global guards read two bits and the base binding
- Tier-0 superinstructions do not pay
- Closures over one body expression share its compiled code (built, with exceptions)
- Top-level loops compile per evaluation, without a call frame or a tier
- No baseline tier, and code images hold bytecode only
- Compiled code calls an active binding at every read
- Compiled code binds GNU R's inlined base functions at compile time
- Tier 1 leaves compiled code through frame states (built, with exceptions)
- An inlined call starts with one leaf helper call
- A body compiles only when its native work covers what compiled code costs
- A compiled return writes locals back only when something can still read the frame
- A call compiled code cannot inline enters the callee in tier 0's dispatch loop
- A native recursive function's depth budget shrinks with the interpreter depth
- Parked: cheaper global bindings in top-level loops (tier 0) (planned)
- A compiled replacement probes the target before any subscript runs

## Heap and collector

[`heap-gc.md`](heap-gc.md): The object model, allocation, the garbage collector, and start-up and code images.

- The runtime's object model, collector and native boundary (built, with exceptions)
- An escape lasts until the outermost native call returns
- Read-only element pointers escape too
- data.table's C runs at or below GNU R's time: barrier, pacing and string kernels
- What keeps a dropped object alive: registers, handles, caches and dying finalizers
- libR commits mimalloc arenas lazily, and handle vectors are not zeroed before their fill
- The young generation allocates by bumping a cursor
- The string intern table is a packed open-addressed array
- Symbols stay in a hash map keyed by their `CHARSXP`
- `gc()`, `gcinfo()` and `gc.time()` report the collector's real counters (built, with exceptions)
- Collections run on GNU R's schedule, and dead large vectors go back to the kernel (built, with exceptions)
- Two old generations, separate vector and node limits, and a pool as large as the vector limit (built, with exceptions)
- Large temporaries: freed from their reference counts, tracing stays for cycles
- Frames released at return
- Start-up memory: release relocation tables and preflight code pages
- The start-up image is build output of the base tree, and every start re-reads the environment (built, with exceptions)
- The start-up image carries the host's environment, regex automata are built lazily, and the heap uses huge pages (built, with exceptions)
- The start-up image is the heap's pages at a fixed address, mapped in place
- Code images: compiled code beside the lazy-load database (built, with exceptions)
- Vectors grow in place into spare capacity
- Card marking, frame watermarks and moved list growth
- Inline small-vector allocation
- Inline marking for minor collections (built, with exceptions)
- Minor collections touch only what changed
- Pretenuring lazy-load fetches
- Lazy-load records are read in place from the code image
- Escalation prediction and the dense precious set
- The start image keeps base R's closures compiled (built, with exceptions)
- The start image is compacted in touch order at write time
- Young collection stress keeps survivors untenured
- Evaluated arguments are rooted, and `gc()` runs queued finalizers first
- Start-up and code images are keyed by build inside a base R tree
- Marking a traced object costs a few table loads (built, with exceptions)
- Peak RSS is judged as anonymous, image and code size, not as one number
- A handler stack set aside is traced

## C API, R home and packages

[`libr-rcmd.md`](libr-rcmd.md): The `libR` C API, `R CMD`, package installation, the bundled base R, and the loader behind `ry load`, `ry test` and `ry document`.

- Packages and R versions in the runtime (built, with exceptions)
- The full C API surface and package installation (built, with exceptions)
- The package-development loader (built, with exceptions)
- The loader on ry's runtime
- libr reads options from base's `.Options`
- One serializer writer, in libr
- A package ry compiled records its headers' binary interface
- ALTREP elements go through the class methods everywhere
- The presented R version comes from the base package the runtime runs (built, with exceptions)
- ry run and ry R: the command chooses the runtime
- Source-change detection lives in the loader, so both runtimes reload the same way
- A reload re-derives one file and rewrites only the bindings it changed
- A cold load is analyzed while base R starts
- ry refuses a `libR.so` built from other sources
- The C headers declare the API of the presented R version (built, with exceptions)
- macOS packages bind to ry's libR
- Windows runtime: `R.dll` and `ry.exe` for windows-gnullvm, linked with lld (built, with exceptions)
- ry ships its own base R, and GNU R through `R_HOME` is the opt-in
- The runtime emulates the GNU R version it loads base R from
- The C API draws from the interpreter's generator
- Native code reads an environment as a cell
- The strict load check overlaps the runtime start (built, with exceptions)
- Every load of a strict package is checked (built, with exceptions)
- Lazy definitions keep source references
- GNU R's public C API is a checked list, not a measured subset
- Windows `R CMD INSTALL`: the home's `Makeconf` decides the platform (built, with exceptions)
- The Windows session's native encoding is UTF-8
- A load holds back S4 definitions and caches its index across processes
- The strict load check shares the check record (built, with exceptions)
- `ry test` is `ry load` plus testthat, with affected tests first
- `ry test --jobs` runs test files in testthat's workers, loaded by ry's loader
- A C API argument that may be a C `NULL` is typed `Option<Sexp>`
- Source edits arrive by OS notification, ordered by a barrier file
- `ry document` is roxygen2 with block and topic caches and linear topic ordering
- A strict reload re-checks through a persistent salsa database
- Compiled code under `ry load` builds with ry's own R home (built, with exceptions)
- The strict load check analyses only the package
- A reload diffs S3 registrations per table entry
- `R CMD` make variables expand as GNU make expands them
- BLAS and LAPACK come from the system, then from GNU R's Makeconf
- `ry test --watch` selects callers through S3 dispatch
- A release is `ry` alone, with the runtime embedded and the R front end folded in
- S-era package sources are R sources
- Start-up and imports run GNU R's code, with its hot loops native
- Installed packages load through native fast paths, not an image
- Package builds run `make` in parallel and link BLAS and LAPACK by versioned file
- ry's base R ships the recommended packages
- Hot C API entry points take a direct success path
- Measured and not taken: a builtin path for `Rf_eval` and a cheaper protect stack
- Measured and not taken: libdeflate for lazy-load records

## Typed ry and the standard library

[`typed-stdlib.md`](typed-stdlib.md): Typed closures and their native compilation, records, tuples and tags, and `std`.

- Beta semantics: Signature matching is name-aware
- Records, tuples and tags are ry values, not R lists (built, with exceptions)
- Inline annotations at run time (built, with exceptions)
- Ry's standard library (`List`, `Str`, `Num`, `Result`, `Dict`, `Set`) (built, with exceptions)
- Standard library updates reuse a uniquely held value
- Standard library breadth against Roc's builtins (built, with exceptions)
- The standard library runs only on ry's runtime (built, with exceptions)
- Nominal and structural types cover both product and sum types
- An Array is a flagged R vector, written `[...]` (built, with exceptions)
- Array storage stays row-major; record Arrays go columnar only with JIT field fusion
- In-place updates through nested targets
- Library leaves resolve at compile time
- Library traversals compile to loops
- The standard library is measured against GNU R, function by function
- Moving a collection into a helper's parameter
- A key of the wrong type is an error, not a missing key
- Records, tuples and tags compile natively
- The standard library is the `std` namespace, with an optional prelude
- Typed closures run as native functions over records, tags and match
- Typed ry has its own numbers, `Bool`, `Str` and `List` (built, with exceptions)
- Typed values are native, immutable, acyclic and freed by reference counts (built, with exceptions)
- Typed code compiles ahead of time, and `ry build --aot --exe` writes an executable
- `ry run --aot` compiles a script's typed code before it starts
- An installed typed package holds elaborated sources with canonical annotations
- The typed code file is position-independent code whose offset table is the per-process table
- `ry build --aot --exe` appends the script and its code file to a copy of `ry`
- The typed core lowers R syntax at the first call and runs on 16-byte strings and view lists
- Typed `I64` runs in registers, and list arithmetic fuses into one loop
- Array subscripts walk offsets instead of listing them
- Typed boxes have shapes, and a tag type is a union of them (built, with exceptions)
- Typed-to-typed calls are eager on every tier
- R lists do not convert to records at a typed boundary (built, with exceptions)
- A typed call that runs in the interpreter always says why
- Typed code calls R natively
- Typed list literals compile natively, and an empty list of boxes settles its item type at first use
- A count of a logical list fuses with the comparisons that built it, after lowering
- A uniquely owned Array grows in place
- A ry list is one item word
- Typed `List.sort_with` is a merge sort with its comparator inlined, and strings order by the interpreter's collation
- Converting a record tree at the typed boundary takes one shape-table borrow and no per-node allocation
- A typed record or tag that R holds stays a box until R reads into it
- The typed suite scores typed ry against Rust
- Nativeness is a strict-mode diagnostic
- Typed code holds table values natively
- A serialized typed closure carries its signature
- A record or tag that stays in its function has no box
- A small typed function is lowered into its caller
- A typed loop over a held list counts a position and reads without a range check
- A typed free calls a register-preserving entry
- Fused list kernels are compiled per CPU feature level and clear no buffers
- `Dict.update` probes once and builds no `Result`
- The typed `sales` benchmark is written over `Dict`
- Short strings are built and hashed as two words
- The typed log reports each compilation's own time
- One thread-local holds every typed allocation pool
- `Str.split_on` with a one-byte separator scans a word at a time
- A branch on a comparison of two doubles compares once
- `List.count_if` with a comparison predicate counts over the whole list
- The typed `array_filter` benchmark counts with `List.count_if`
- Shapes are confirmed after lowering, and machine code is generated once
- `List.sum` adds doubles in order in double precision
- Counts that share a list share one kernel pass (built, with exceptions)
- Parsing a number is one instruction with two results
- An `I64` remainder or quotient by a positive constant needs no checks
- A split list that is only read at constant positions is never built
- A loop that fills a list is fused with the loop that reads it
- Whole-number ranges decide which checks typed code needs

## DataFrame and the table engine

[`frames-table.md`](frames-table.md): The built-in DataFrame, its kernels, CSV reading, and the data.table boundary.

- The built-in frame (built, with exceptions)
- Frames as runtime values (built, with exceptions)
- data.table runs on its own code; the `[` bridge is removed (built, with exceptions)
- Delimited files: one sniffer for the reader and the checker (built, with exceptions)
- The table engine's sort and distinct kernel
- The DataFrame API is Polars', and ry's own types are UpperCamelCase (built, with exceptions)
- Per-group top rows and as-of joins reuse the engine's kernels
- data.table's helpers run data.table's own C, and the C API is what makes them fast (built, with exceptions)
- Frames and the checker share one CSV reader, and data.table keeps its own (built, with exceptions)
- A DataFrame column reads back as a plain vector, not an Array
- The engine's workers return freed memory at large frame boundaries
- Group codes wider than 64 bits are ranked, not handed to Polars
- Double sums group by code range, one scan per thread
- Factors are ry's own codes plus one level table, outside Polars
- A factor's level table is front-coded in byte order, with no hash index
- Equality joins bucket one side by dense codes of its own key range
- A filtered `:=` reads a row index and writes in place
- A query without `by` aggregates in the kernel as one group
- DataFrame expression operands are checked against the engine's own typing
- Strict mode reports a DataFrame query over unknown columns
- CSV files are read in released slices straight into the final column buffers
- The Rust table benchmark runs on libR's allocator and the same lazy plans as the R path
- Table operations map their large blocks on huge pages
- Group-bys and joins run on ry's kernels, not Polars' streaming engine
- The data.table migration is one engine in `ide`, read off the checker's types
- Dates, windows, ranks, cross joins and pivots in the table engine
- A `NULL` local inside a query of unknown columns is the column
- String keys sort by rank, and merge pairs come from sorted codes
- package:ry attaches no bare name another package exports

## Build, benchmarks and CI

[`tooling-bench-ci.md`](tooling-bench-ci.md): Build profiles, releases, platforms, the dev shell, CI, fuzzing harnesses and benchmark suites.

- Every pipeline stage is fuzzed from its first commit (built, with exceptions)
- The shipping binary links no Apple frameworks (built, with exceptions)
- The Linux release binary links glibc 2.17 through zig (superseded by "Releases are built natively per platform, with the runtime beside the binary")
- The benchmark harness compares test by test through `ry load`
- Everything is fuzzable, simulatable and scriptable
- `just ry` builds ry's runtime; ry-lang does not depend on libr
- Releases are built natively per platform, with the runtime beside the binary (built, with exceptions)
- The bench templates are inline-typed
- Seeded batteries replay through one loop
- Every seeded harness rotates through `FUZZ_SEED`
- CI runs `just gate` against GNU R 4.6.2, and nothing ignored (built, with exceptions)
- The tests that need no GNU R run by hand and gate a release, never on push (planned)
- Optimized tests build with the bench profile, not fat LTO (superseded by "Build profiles: release for numbers, iterate for loops")
- How bench-real measures (superseded by "Four published benchmark suites, one runner")
- Published benchmark numbers come only from the results files (built, with exceptions)
- Benchmark suites are directories the renderers only display (built, with exceptions)
- Four published benchmark suites, one runner (built, with exceptions)
- Flake inputs are git URLs (built, with exceptions)
- Cloud sessions are provisioned by tools/dev-setup.sh, not the nix shell
- CI runs in the flake's dev shell
- Workspace-wide feature unification
- Prebuilt trees come from the ry.cachix.org binary cache
- Build profiles: release for numbers, iterate for loops
- Worktrees keep their own target directories and share sccache
- Published benchmarks measure the release archive
- A fatal signal prints one report from the executable, which asks the runtime for the R stack
- Platform code is type-checked from Linux before the release job builds it
- The dev shells' GNU R is cached on every change to its inputs
- The hot suite replays the package entries the code bases spend their time in
- The code bases are a benchmark suite of their own, outside the headline
- The profile summary hashes private package names with a secret key per project

## Docs, process and positioning

[`docs-process.md`](docs-process.md): The docs site, licensing, positioning, and how agents record and organize knowledge.

- Licensing (built, with exceptions)
- Positioning (built, with exceptions)
- Data-oriented design and explicit allocators
- Docs information architecture: Introduction, Learn, Reference, Development
- Landing-page and README samples are captured from the running tools
- The decision log is one file per area, with a generated index and byte budgets
- Source comments have length limits, enforced by a ratchet
