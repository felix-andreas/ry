# Decisions: R compatibility

Matching GNU R's observable behavior: the oracle, corpus package tests, and R semantics details.

## Package tests through the oracle

**Status:** built.

`oracle packages` (`crates/oracle/src/packages.rs`, `development/corpus-and-oracle.md`, "Package
tests") runs each corpus package's own `tests/` under GNU R and ry. It is the measure for "the corpus packages run on ry".

- A file passes when its process exits 0 and a package when every file does (`R CMD check`'s rule).
  `.Rout` files are not diffed, because they carry timings, paths and addresses; testthat's summary counts are read from them and reported beside the verdict, showing how far a failing package got.
- The pass rate counts only packages GNU R passes, since a GNU R failure (missing suggests, network, a display) says nothing about ry; GNU R's results are reused per package version
  unless `--rerun-gnu-r`.
- Each file runs in its own process group, killed as a group on `--timeout`, because a hung test
  leaves a callr/processx child holding the pipe.
- Run against a copied binary (`ORACLE_RY`), since a rebuild under a long run changes the engine.

## Corpus package tests: compatibility details that are easy to get wrong

**Status:** built.

- **Hashed frames list in GNU R's bucket order** (`ls()`, `as.list(env)`, `mget`, `env.profile`),
  which snapshot tests record. The order depends on the frame's whole history (GNU R's table never
  shrinks on `rm()`, and its chain counter falls only when a bucket empties), so replaying live
  inserts cannot reproduce it. A hashed frame therefore keeps GNU R's chained table itself
  (`HashTable` in `crates/heap/src/env.rs`): PJW hash, insert prepends and counts one (GNU R's `R_HashSet` counts every insert, since its
  empty-chain test runs after the scan), grow past 0.85 to `size * 1.2 + 1`, moving buckets in
  ascending order and recounting occupied buckets. It is updated on every insert, removal and
  compaction. It is the single source of truth for listing; lookups use the
  address-keyed index. The global environment is hashed from creation. Checked against a plain model by
  `crates/heap/tests/test_frames.rs`.
- **Valid UTF-8 is marked UTF-8** by `Encoding<-` and `iconv` even when asked for "unknown",
  because strings intern by pointer and a second pointer for the same text breaks `==` (desc's `read_dcf` broke on duplicate fields). Latin-1
  strings stay separate; `==` compares them by text (`values::same_text`), `match()` and `unique()`
  do not yet (`backlog.md`).
- **Names and symbols compare as text across encoding marks**, as GNU R's `installTrChar` and
  `Seql` do: `install_charsxp` keeps a valid UTF-8 or ASCII `CHARSXP` as the symbol's name and
  translates a latin1 one to UTF-8, the heap's symbol table finds a spelling under either mark,
  string-literal tags (`c("aä" = 1)`) keep their UTF-8 mark, and `[`, `[[`, `$` and their
  replacements match names through `values::same_string` (`subset__non_ascii_names`).
- **A sink's connection is a GC root** (`connections::Table::trace`), as GNU R preserves it;
  otherwise `capture.output` finalized it.
- **S4 dispatch on an unsupplied argument dispatches on NULL**, even with a default, as GNU R's
  `GetObject` does (`dispatch::dispatch_argument`).
- **Queued finalizers root their weak reference's key, value and finalizer**
  (`libr::boundary::root_finalizable`): the collector never traces weak reference fields and an
  earlier finalizer can allocate. `RY_GC_VERIFY=1` checks the write barrier before every minor
  collection, which tells a missing barrier from a missing root.

## Matching GNU R's own tests where its behavior is an artifact

**Status:** built, except the black-box rule and the `smooth.spline` and `loess` notes, which "Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate" supersedes, and the long double `List.sum`, which "`List.sum` adds doubles in order in double precision" supersedes.

GNU R's `tests/*.R` pin behaviors that come from its machine or implementation, not the language.

- **x87 long double sums.** `sum`, `mean`, `rowSums`, `colMeans`, `var`, `cov` and `cor` accumulate
  in an 80-bit model, because only an exact model agrees bit for bit (`mean(c(1:9, -1e300, 1e300))`
  is 0). On x86_64 `long_double::add_all`, `add_differences` and `add_products` run on the x87
  through inline assembly (System V leaves precision control at 64 bits); the software model
  (about 10x slower, 13 ns an element) is the fallback elsewhere, on Windows (its CRT sets 53 bits) and outside the
  x87 exponent range. `sum` over several double arguments rounds each argument's total to double
  and adds it to a long double total, which GNU R's results need. The data.table bridge keeps plain
  double sums for GForce shapes.
- **`%%` and `%/%`** correct the remainder in extended precision, handle a divisor too large for the quotient, and
  warn past 2^63, matching GNU R's observed results on huge operands.
- **Invalid UTF-8 bytes** become private-use code points `U+10FF00 + byte` while `interp` holds a
  string as a Rust `str`, and print as `\xNN`; the heap keeps exact bytes and the encoding flag.
  Carrying bytes through every string builtin (about 230 readers) was rejected: no observable gain.
  The tail of the last plane is used because the earlier `U+F780..` range collided with symbol
  fonts; a genuine character there is the one misread.
- **A non-ASCII "bytes" string is refused where GNU R calls `translateChar`** (`values::refuse_bytes`:
  case changes, `chartr`, `sprintf`, `make.names`, `abbreviate`, multi-element string
  `sort`/`order`, `as.vector(mode = "symbol")`); `nchar` has its own messages. `paste`, `substr`,
  `strsplit`, `format` and `useBytes` regex work on the bytes (`strings__bytes_encoding`).
- **A bytes-marked pattern or text forces `useBytes = TRUE`** in `regexpr`, `gregexpr`,
  `regexec`, `grep`, `sub`, `gsub` and `strsplit`, as GNU R does.
- **TRE compile errors** reject PCRE-only groups (`(?<`, `(?=`, `(?!`, `(?>`, `(?P<`) with
  "Invalid regexp" and an inverted interval with "Invalid contents of {}", with GNU R's wording on
  its three paths (wide-character `regexpr`/`grep`, `regexec`, the rest).
- **`agrep`/`adist`** use an edit-cost NFA; `adist`'s `counts` traceback prefers GNU R's observed
  order.
- **Scripts run as console input**, one top-level expression at a time, with `stdin()` reading the
  rest of the script, because GNU R's tests read data from their own source.
- **`sink(type = "message")` diverts every stderr writer**, top level invokes an R-level `abort`
  restart, and messages are cut to `warning.length`.
- **`.Last.value` holds no reference count**, so binding it forces no copy.
- **Replacement primitives called directly** (`` `names<-`(x, v) ``) copy their target; only a
  complex assignment owns it.
- **`pretty()` and axes near the double range**: the 4% extension clamps to ±`DBL_MAX`, log axes stay
  within `log10(0.99*DBL_MAX)`/`log10(1.01*DBL_MIN)`, a zero log limit becomes -320 with GNU R's
  warning. libR's `GEPretty` and `Rf_CreateAtVector` reuse `linear_axis` and `at_vector`, so grid
  and base graphics share one tick algorithm.
- **Deparse parenthesizes dangling operands** (an `if`, loop or low-precedence unary ending a binary
  left operand, GNU R's PR#18232). GNU R leaves `function` bare there, which reparses differently;
  ry keeps the parentheses on purpose. A braced `if` stays on one line as an argument of a
  primitive call (`c`, `list`, `quote`, `return`) and breaks inside closure calls, `[` and list
  elements; `else` after a braced consequent starts its own line.
- **Accepted divergences**: `fisher.test` beyond FEXACT's hash computes the p-value instead of
  erroring; `gsub("\\b", ...)` skips TRE's re-match after an empty match; a bytes string pads by
  its escaped width; `pf(x, 1, 1, ncp = 20, lower = FALSE, log = TRUE)` is the more precise
  -17.1486 (GNU R -17.1318, `d-p-q-r-tst-2` line 302).
- **`nlminb` control ranges** are PORT's `DPARCK` checks (`iv[1]` names the failing position, so
  GNU R's "is out of range" prints); `nls(algorithm = "port")` keeps a simplified check.
- **`qbeta`** polishes its last digits by walking adjacent doubles to minimize `|pbeta(x) - p|`
  (GNU R's tests need mean relative round-trip error below 8e-16). Past the log-scale solve, Newton
  steps run on the caller's scale; a NaN from pbeta's asymptotic expansion bisects toward 0;
  search warnings are dropped; a subnormal result snaps to 0 or 2^-1024, because GNU R never
  returns a subnormal quantile.
- **`dpois` deviance in double-double** (`ebd0` in `rmath.rs`, own atanh-series log), because tests
  pin `dpois(1400, 1000)` to a few ulps.
- **Discrete quantile slack**: within 64 epsilon of the target measured from the nearer end of
  [0, 1] (16 epsilon on the log scale). Slack relative to `p` alone missed near 1; narrower exposed
  `ppois` digit differences.
- **`qnchisq` stops at relative accuracy 1e-13**: an exact answer drops out of `all.equal`'s mean
  and failed `d-p-q-r-tests` line 188.
- **`zeroin2` reports `|c - b|` as precision after the iteration limit**, not -1 (`uniroot`'s
  `estim.prec`).
- **`traceOnOff`** keeps a real tracing state, default on, so `trace()` tracers run.
- **`Fexact`, `parseLatex`, `gzcon`, `OutDec`, classed `parseError` conditions and
  `file(encoding=)` re-encoding** are native implementations of the documented interfaces, checked
  against GNU R's tests.
- **S4 arguments to `cbind`/`rbind`** go to `methods:::cbind`/`rbind` with `deparse.level` tagged;
  level -1 marks methods' own `cbind2`/`rbind2` defaults and binds directly.
- **Start-up takes each locale category from the environment, as GNU R does** (`system::init_locale`).
  A category the system cannot provide falls back to "C" and start-up warns `Setting LC_X failed,
  using "C"` after every other start-up warning, so `l10n_info()` and `iconv(..., "ASCII//TRANSLIT")`
  agree with the C library. Forcing C.UTF-8 instead claimed UTF-8 that GNU R does not have there.
- **URL transfers use `reqwest`, not libcurl** (`crates/services/src/http.rs`), already linked
  through polars; it honors the proxy variables and the platform's roots (`SSL_CERT_FILE`). A `url()` connection fetches its whole body on open and reads from memory, like `unz()`. Failures use libcurl's codes
  and wording, which R code matches on. Each request runs on a scoped thread
  (`on_transfer_thread`), because the blocking client panics inside an embedder's async runtime; a transfer thread that
  dies reports libcurl's "Failed initialization" instead of unwinding into R.
  `curlGetHeaders` follows redirects hop by hop (no proxy CONNECT response). `libcurlVersion()`
  reports 8.5.0, the release whose behavior the transfers follow, so package version checks pass; its attributes say what ry uses (`ssl_version` "rustls", no
  `libssh_version`, protocols file, http and https).
- **Sockets are blocking `std::net` streams** (`Device::Socket`/`ServerSocket` in
  `crates/interp/src/connections.rs`). A non-blocking R connection polls with zero timeout and
  marks itself incomplete when empty, which is how `readLines` holds a partial line; a blocking
  read waits `socketTimeout()`. Each read refills a 64 KiB buffer with one receive, so `socketSelect()` counts buffered input as
  ready without asking the OS; polling is `platform::poll_sockets` (`poll(2)`, `WSAPoll`).
  Deliberate differences: `serverSocket(70000)` errors where GNU R truncates, and
  `socketAccept(timeout =)` gives up where GNU R waits forever. `parallel::makeCluster()` works
  (`parallel__socket_cluster`); workers start `R.home("bin")/Rscript`, so they run ry only under
  ry's R home (`just ry-home`); under plain `ry run`, whose `R.home()` is GNU R's, they are GNU R
  processes speaking the same serialization.
- **Unserializing from a connection reads one object** (`unserialize_connection`,
  `crates/interp/src/serialize.rs`), so a peer's next message stays unread. An ASCII string reads
  exactly its length, leaving the newline unread as GNU R does. Trap: an ASCII object that ends in a
  string is therefore followed by that newline, which only a following ASCII header (`\nA\n`) accepts.
- **`warning(immediate. = TRUE)` signals like any warning**; a flag on the interpreter makes only the
  default handler print it at once, so calling handlers and `assertWarning` still see it.
- **`Cstack_info()`** reports a fixed 7969177-byte size, current use from the remaining native stack and the
  evaluation depth from the frame stack; scripts only compare these against each other.
- **`sort.list` is never an S3 method of `sort`**: dispatch skips base's own `sort.list` closure,
  because GNU R treats it as a plain function and `sort(x)` on a classed `x` must not reach it.
- **A generic called without its first argument dispatches as on NULL**, matching `UseMethod`.
- **An S4 value assigned into a non-list vector** goes through `as.vector` first; into a list it is
  embedded with GNU R's "implicit list embedding of S4 objects is deprecated" warning.
- **JIT function speculation skips frame-bound symbols**: a call of a symbol the frame can bind
  (a formal or a local assignment) is never speculated from the enclosure, because a formal holding a function
  shadows a global of the same name (`uniroot(f, ...)` with a global `f`; `formal_shadows_enclosing_function` in
  `crates/jit/tests/test_tiers.rs`).
- **One implementation each**: parse data comes from `crates/interp/src/parse_data.rs` (replays GNU
  R's lexer contexts); hashed chains only in `HashTable`.

## Runtime semantics settled by the corpus package tests

**Status:** built.

- **A loop accepts a `break`/`next` from another call only when GNU R's compiler would give it a
  context**: its body calls `eval`, `evalq` or `source`, or writes `break`/`next` inside an ordinary
  call. Otherwise `eval(quote(break), parent.frame())` reports "no loop for break/next". Each loop
  records its call depth and whether it has a context, and compiled closures apply the same test (`base__loop_jumps_from_other_calls`).
- **One implementation per attribute setter.** libr's `Rf_dimnamesgets` and `Rf_setAttrib`'s
  `row.names` arm call the interpreter's `set_dimnames`/`set_row_names`; a drifted libr copy missed
  the `c(NA, -n)` form and hung readr's chunked reader.
- **A user interrupt is a condition first.** `Rf_onintr`, `R_CheckUserInterrupt` and the
  evaluator's poll call `conditions::signal_interrupt`, which offers class
  `c("interrupt", "condition")` to calling and exiting handlers and unwinds to top level only when
  none takes it, as GNU R's `onintr`. later and rlang's `interrupt()` rely on it.
- **A pending interrupt waits for the next interrupt check.** The interpreter owns
  `INTERRUPTS_PENDING` and `INTERRUPTS_SUSPENDED` under GNU R's names, so C code,
  `suspendInterrupts()` and the evaluator share one record; `INTERRUPT_REQUESTED` only means
  "attend at the next poll". The evaluator's poll (calls, loop iterations, compiled back edges through deoptimization)
  delivers on every 1000th poll (GNU R's `eval` cadence);
  `R_CheckUserInterrupt` delivers at once unless suspended; `Sys.sleep()` and socket waits deliver
  even while suspended (GNU R's `R_SelectEx` lifts it). Leaving `suspendInterrupts()` delivers
  nothing by itself; a C context saves and restores the suspension; `ry run` installs the SIGINT
  handler. Gap: a host setting `R_interrupts_pending` directly is seen only by
  `R_CheckUserInterrupt`, not by the evaluator's poll, which reads the attention flag. Pinned by `crates/libr/tests/test_interrupts.rs` (own process) and
  `conditions__suspended_interrupts`, `conditions__interrupt_handlers`.
- **ALTREP vectors are first-class at GNU R's thresholds.** Every `1:n`, `seq_len`, `seq_along` of
  two or more elements is a compact sequence (`sequence` in `crates/interp/src/arith.rs`);
  `as.numeric` of one stays compact; `as.character` of an attribute-free number vector is a
  deferred string; `duplicate` of 64+ elements that changes only attributes makes a wrapper
  (`values::shallow_duplicate_attr`, `WRAP_THRESHOLD`). `inspect` and data.table's ALTREP tests
  (754.07, .08, .10) therefore match GNU R. Lower thresholds than GNU R's (2^20 for sequences, 256 for deferred
  strings, no wrappers) were dropped: they existed only because tier 0 and the JIT read plain data
  in place, which the placement rule makes cheap.
  - *Placement rule.* A plain read pays one ALTREP bit test; every ALTREP branch is
    `#[cold] #[inline(never)]`. Tier 0's in-handler paths must not run R code or collect (a
    package class's `Elt` may), so tier 0 reads only compact sequences inline
    (`values::compact_sequence`, `Interp::compact_sequence_classes`) and deferred strings through
    `values::altrep_string_elt` (gated on `Interp::deferred_string_class`, which runs no R); any
    other ALTREP read declines to the builtin (`element_access::altrep_element`). Cost: a compact
    read in tier 0 is about 110 to 190 Ir more per iteration than a plain one (not yet within 20
    Ir), against 1,000 to 1,900 more when it declined to the builtin; a plain read costs nothing
    extra (callgrind on `acc <- acc + x[i]`).
  - *JIT.* Attribute-free compact integer and double sequences type as `Ty::IntSeq`/`Ty::DblSeq`
    (feedback bits `INTEGER_SEQUENCE`/`DOUBLE_SEQUENCE`; feedback words are u32 to fit them), and
    `for`, `x[i]`, `x[[i]]`, `length(x)` compute from the `c(n, n1, inc)` info
    (`sequence_info` in `crates/jit/src/codegen.rs`). Compiled code embeds the class pointer, so it
    is valid only in its process. Other ALTREP values side exit on `OBJECT|ALTREP`. The double
    form matters because `as.double(1:n)` stays compact; without it `x[i]` was a boxed builtin
    call at 1,273 Ir per iteration.
  - *Sequence speculation never mixes with a plain vector.* `a:b` with `a == b` is a plain scalar,
    so the sequence guard also accepts a non-ALTREP length-one vector. A sequence joined with a
    plain vector is `Boxed`, never `IntVec`, because the slot join keeps a sequence one path forgot
    (`inds <- 1:n` on one path, `inds <- which(...)` on the other); load feedback that saw only one kind types the variable as the plain
    vector, so `perm1 <- 0:(n-1)` followed by element stores stays `IntVec`. A variable the body assigns into is never a sequence: `SetVar` stores an expanded copy
    (`jit_sequence_contents`), and a load finding a compact value copies it and marks the slot an
    outer variable so the first element store writes back. Converting on a join edge was rejected:
    an edge has no side exit and the copy would need its own reference ownership. `for` normalizes
    its sequence register to the plain type. Without these, fannkuchredux and fasta_3 recompiled their main body up to
    four times (`jit__sequence_speculation`).
  - *Reads leave a sequence compact*, because GNU R's `print`, `[`, `[[` and `for` read through
    `*_ELT`. Printing reads into a fresh vector (`values::read_contents`); `[` on a compact sequence
    takes the element path, `[[` reads through `Elt`; the tree-walking `for`
    reads element by element (top-level `for (i in 1:1e9)` stays at 43 MB, not 3.9 GB)
    (`altrep__reads_keep_compact`).
  - *Arithmetic reads a sequence unexpanded* through a `Sequence` reader in
    `crates/interp/src/kernels.rs`. This deliberately differs from GNU R, which leaves the operand
    `(expanded)`; values match (`altrep__sequence_arithmetic`), memory is saved, and no case pins
    the inspect state after arithmetic. `abs` and `%%`'s accuracy check still expand.
  - *Summaries ask the class first* (`Sum`/`Min`/`Max` via the `altrep_summary` hook), then read a
    compact sequence without expanding: `min`/`max` read the ends; `sum`, `prod`, `all`, `any`,
    `mean` read into a fresh rooted vector (`SequenceRead`); `range` expands, as GNU R's
    `range.default` calls `c()`. Compare with GNU R one process at a time: Rscript folds a `3:12`
    in a compiled loop into a shared constant.
  - *Wrappers.* Assigning a sole-owner wrapper unwraps it (`values::unwrap_assigned`). A shallow
    duplicate shares the wrapped data; every ALTREP instance is not mutable (refcount 65535 in
    `new_altrep`), so writes copy first. Wrapped data keeps the attributes it had
    (`DUPLICATE_ATTRIBUTES`), so `materialize_contents` reuses a stored slot only when it has no
    attributes (else xts's `unclass(x) - 1` dispatched `-.yearmon`). `memory::duplicate` without a
    `Duplicate` method returns fresh storage, because C code writes into duplicates (data.table's
    `frev`/`setkey` corrupted `1:n`).
  - *Sparse reads.* `x[i]` on a non-sequence ALTREP of 2^16+ elements reads element by element when
    the index selects at most an eighth (`reads_sparsely`, `crates/interp/src/subset.rs`)
    (`base__long_sequences` keeps `seq_len(2^31 - 1)` under 1 GB peak). `x[[i]]` and a sparse `x[i]`
    read through `values::int_elt`/`real_elt`.
  - *Remaining differences.* `inspect` prints `REF(65535)` for wrappers and deferred strings; the
    JIT side exits on deferred strings, wrappers and package classes (an extern element reader is
    the follow-up); arithmetic on a wrapper over attributed data materializes, where GNU R reads
    through the data pointer. Lists have a `wrap_list` class, as in GNU R 4.6.
  - *The JIT reloads a sequence's info on every read.* Per-slot SSA variables were rejected: slot
    status can be not-loaded at an OSR loop head, so they become block parameters that cost more
    than the reload. Speculating the representation in the slot guard and folding the bound check into the
    index compare saved nothing measurable. `known_object` reuses a read within a fall-through
    chain (`Plan::continues` in `crates/jit/src/analysis.rs`). The floor is a flag test and branch,
    the info pointer, three loads, a conversion, a multiply and an add (8 to 10 instructions over a
    plain load), and Cranelift hoists no loop-invariant work, so convolution runs about 1.06 to
    1.08x the instructions of plain vectors and nbody about 1.01x. Expanding on the first compiled read would close the remaining
    gap but diverges from GNU R, which keeps the sequence `(compact)`.
  - *Only user-visible `as.double` keeps a sequence compact* (in `as_type`,
    `crates/interp/src/coerce.rs`, not `coerce_vector_quiet`): a builtin's own coercion expanding
    while it holds unrooted results crashed `complex(real = 1:2, imaginary = 3:4)` under GC stress.
  - *A new compact sequence is not rooted* (`new_sequence`, `crates/libr/src/altrep.rs`): a handle
    per `a:b` would pin each until the call returned; the allocation keeps its info vector alive.
  - *`a:b` compiles without the builtin call* (`Intrinsic::Colon` → `jit_colon`, NULL for doubles,
    bad bounds or a heap error so tier 0 raises those); a double-feedback site keeps the builtin. The builtin call cost about 1,250 Ir per `a:b`, more
    than the sequence's whole creation, which is why the sequence's second allocation was not
    folded into the instance.
  - Pinned by `altrep__wrappers`, `datatable__compact_columns` and data.table's suite.

## Package-test compatibility details that look optional but are not

**Status:** built.

- **Warnings always go through `.signalSimpleWarning`** when it is bound, even with no handler, as
  GNU R's `vsignalWarning`; rlang reads the frames it leaves (`warning_call` in
  `crates/interp/src/conditions.rs`).
- **Compiled closure calls pass constant arguments as values**, not promises (GNU R's
  `PUSHCONSTARG`); rlang's `env_dot_type` tells them apart. `compiler::cmpfun` compiles at once
  (`bcClose`, `eval::collect_arguments::<COMPILED>`).
- **`dyn.load` refuses GNU R's `tcltk.so`**: GNU R's base package objects read `SEXPREC` fields
  directly and crash on ry's heap; the others are built into libR (`builtin_libraries.rs`).
  The refusal says "Tcl/Tk support is not available on this system", which sessioninfo's
  `external_info()` catches; `capabilities("tcltk")` is `FALSE`.
- **Parse errors use GNU R's exact text**, which testthat matches: the token bison's LALR parser
  stops on, with its token names, a 1-based column with tabs to multiples of 8 (end of input is
  column 0 on the next line), and the last 255 bytes as context with the last two lines quoted
  with a caret. `Rscript` and the console quote only the failing top-level expression
  (`grammar_error_report` in `lower.rs`; `parse__error_messages`).
- **`list.dirs` names the directory itself only when `recursive = TRUE`**; testthat's snapshot
  variants otherwise got a phantom `""`.
- **Base binds `R_MissingArg` to itself in the C API**, so binding queries answer for it in base
  frames (`base_binds_missing_arg`, `crates/libr/src/environments.rs`).
- **A compiled `local(expr)` is `(function() expr)()` with a four-element `function` call** whose
  srcref slot is NULL; rlang's `trace_back` writes into it.
- **`stdout()` is the current sink target** (GNU R's `R_OutputCon`); a `stdout()` saved before the
  sink writes past it to the console, connection 1 is never redirected (vctrs' reporter tests broke on
  a stray `.` written through a saved `stdout()`).
- **A connection's `conn_id` address is never compared**, only checked non-NULL, because curl
  stores its own handle there (`resolve_number`).
- **TRE lazy quantifiers with capture groups take the leftmost-first match** (`x(.+?)(a+)` on
  `xaaab` is `xaaa`, `x.+?a+` is `xaa`); openssl's PEM splitting needs it (`Matcher::find_at`). Without groups TRE returns the shortest
  overall end. `\Q...\E` quotes a literal run.
- **`attach(env)` attaches a copy**, leaving the original's enclosure alone (`do_attach`); shiny's
  namespace lost its imports otherwise.
- **A user interrupt is signalled as a condition first**: see "Runtime semantics settled by the
  corpus package tests".
- **`rmultinom` carries the remaining probability as double-double** (GNU R's `long double`); in
  `f64` ten 0.1s tip `rbinom` into drawing the complement, and RcppArmadillo compares draws.
- **`[` subsets a parsed expression's `srcref` list**; plumber needs `attr(exprs[i], "srcref")`.
- **`Rf_setAttrib(R_NilValue, name, R_NilValue)` succeeds**: GNU R checks removal before rejecting
  `NULL`, and data.table's `C_unlock` relies on it (`set_attrib`).
- **`row.names<-` stores a real `c(NA, n)` as integers**, as GNU R; cpp11 reads only the integer
  form and readr loops forever on the real one.
- **`R CMD INSTALL` of a directory copies the shared objects back into its `src/`**: the build runs
  in a copy (so a Cargo workspace never adopts it), but `pkgbuild::compile_dll` and
  `pkgload::load_all` expect the in-place result (`install_source`, `crates/rcmd/src/rcmd.rs`).

## Runtime error checks follow GNU R's order and blame

**Status:** built.

- Where GNU R checks a condition before matching or dispatching, ry checks at the same point and
  blames the same call. `NextMethod()` errors through the innermost closure call ("called from outside a function",
  "called from an anonymous function", "object not specified" when `.Class` is unbound and the
  object has no class, "generic function not specified", "no method to invoke") with no fallback
  to the call's name; `round()` and `signif()` count arguments before
  matching and blame the calling closure, so `eval.rs` leaves their formals check to them.
- **Why.** Packages and GNU R's tests compare these messages, and a lenient fallback hid a
  `NextMethod` that picked the wrong generic.

## A formula's dot expands to the sum GNU R prints

**Status:** built.

- `stats::models::expand_frame_dot` replaces `.` reached through formula operators with the sum of
  the other columns, parenthesized unless the dot stands at the top, inside `(`, as a call argument
  or as the left operand of `+`, which reproduces `terms()` output and deparse.

## The corpus's waiting is the suites' own sleeps, and child R processes run ry

**Status:** built.

- In ry's R home, `bin/R` and `bin/Rscript` exec `bin/exec/ry-rcmd`, so callr and processx
  children run ry (`exists(".ry_view", baseenv())` is `TRUE` in a child). `Rscript -e` starts
  in about 32 ms against GNU R's 140 ms, and a `callr::r()` round trip takes 0.17 s against 0.25 s.
- Main-process waiting in the corpus suites is the tests' own `Sys.sleep` (foreach 77% of its
  run, lgr 22%, cachem 10%), which costs GNU R the same. rlang and purrr set `Config/testthat/parallel: true`, so their work runs in a callr worker
  even with `TESTTHAT_CPUS=1` and the main process sits in processx's `poll` (a 90% waiting share
  that is really ry work and speeds up with it); profile the worker with
  `RY_PROFILE_SUMMARY_DIR` (one summary per process).

## The package corpus runs offline

**Status:** built.

- `NETWORK_TESTS` in `tools/bench/tooling/packages.py` lists each excluded test file with its
  reason (jsonlite's `test-network-Github.R` lacks `skip_on_cran()` and opened 7 api.github.com URLs
  at about 0.2 s each on both engines, which dragged its ratio from 1.52x to 1.26x), and
  `unpack_tests` deletes them.
- Every engine runs with the proxy variables (both cases) pointing at a local proxy that answers
  403 and records the host, `no_proxy` for loopback; any recorded host fails the run before
  `--write`, so a new network test cannot slip in. GNU R's libcurl and ry's `url()` both honour the
  variables (checked with `readLines(url(...))` over http and https).

## The oracle suite names the GNU R build it was recorded with

**Status:** built.

- `crates/oracle/cases/gnu-r.txt` names the release and the BLAS and LAPACK library file names
  (so an install prefix does not matter), asked from R (`oracle::gnu_r_build`). Another release refuses `verify` and `record` and skips the
  recorded-outcomes test (its outcomes are another release's semantics); other libraries only
  warn, because they move last digits. Every report prints the build it ran. One file per suite,
  not a header per `.expected`, because a suite is recorded with one build. Why: a host with 4.3.3 on `PATH` once
  blessed 4.3.3 output into the 4.6.2 suite.
- **Library-dependent cases** (BLAS, LAPACK or libm results printed to the last bit, where an
  optimizer's counts and warnings move too; exact comparison failed on every OpenBLAS host,
  CI's included) carry `# oracle: library-dependent` (`oracle::library_dependent`) and
  are skipped when the libraries differ from the build file; every other case is compared exactly. libm is not in the file (glibc's
  x86-64-v3 build moves `pgamma`, `fft` and `nlm` digits), so those cases compare only where BLAS and LAPACK match too. A tolerance was rejected because iteration
  counts and warnings move, not only digits.
- **A GNU R killed by a signal** is `EngineError::Crashed` and the case is skipped with the build
  named (a hardened 4.6.2 aborts with "buffer overflow detected" on four graphics cases the reference
  source build runs); a crashing ry engine is a failure.
- The forced oracle runs ry on the live R's base, so on a 4.3.3 host it tests 4.3.3 emulation; set
  `ORACLE_R` to a 4.6.2 build.
- **Last digits depend on libm and FMA contraction**: glibc's x86-64-v3 build rounds `hypot`
  differently on about one pair in eight, and `-march=x86-64-v3` fusion moves `df`, `dnchisq`,
  `pnchisq`, `qnchisq`, `pgamma`, `fft` and `nlm`. The libr test packages compile with
  `-ffp-contract=off` (`NO_CONTRACTION` in `crates/libr/tests/harness.rs`);
  `crates/libr/tests/test_numerics.rs` fingerprints `hypot`, matched against the header of
  `rmath_expected.tsv` by `expectations_are_current`. To reproduce v3 on a baseline machine, point
  a test binary at Ubuntu's amd64v3 `libc6` with `patchelf`.
- A portable case includes every header in `R CMD SHLIB` code, sends `?=` for an empty query
  (libcurl 8.8+ drops a bare `?`), and does not print `system()`'s attributes (Docker's seccomp adds
  "Function not implemented").

## The oracle gives every engine the reference R's library paths

**Status:** built.

- `library_environment` in `crates/oracle/src/oracle.rs` asks the reference R for `.libPaths()`
  once (with `R_LIBS_SITE`/`R_LIBS_USER` empty, `R_LIBS` kept), and every engine run, including the
  `installed_packages` probe, gets `R_LIBS` set to that list and the other two empty. ry adds GNU
  R's site and user libraries only to an unset variable, so empty turns them off. `just doctor`
  compares the packages `ry run` would load with the reference R's and prints the three variables
  when they differ.
- **Why the harness and not ry's defaults.** Searching other GNU R installations' libraries is how
  an installed ry finds the user's packages (`reference/cli.md`, "How ry finds R"); the oracle
  compares against one fixed R, so it pins that R's libraries. Without it ry found no packages
  in a container with only `/opt/R/4.6.2`, or loaded Debian's older `/usr/lib/R/site-library`
  versions of data.table, rlang and withr.

## Known oracle failures live in a committed file

**Status:** built.

- `crates/oracle/known_failures.toml` lists each case and engine main fails, with the area and a
  one-line reason (`provisional` while seen only on a branch). `oracle compare` (a comma-separated
  engine list, one summary line per engine) and `ry_matches_recorded_outcomes` report a listed
  failure as known and fail on an unlisted failure and on a listed case that passes, so a
  fix and its entry's deletion land together. Unknown engines, missing cases and duplicates are
  refused. `ry-jit-gctorture` (`RY_JIT=stress` plus `RY_GC_STRESS=1`) is its own engine because a
  dropped root in compiled code shows only with both on.

## A crash is never a known failure

**Status:** built.

- A case whose engine is killed by a signal prints `CRASHED` and is `New` under every engine,
  listed or not (`CaseRun` in `crates/oracle/src/oracle.rs`). `KnownFailure` has no `intermittent`
  field and `parse_known_failures` refuses the key. Why: an "intermittent" mark once recorded a
  memory-safety bug (an unwind parked in an activation the collector did not trace) as host noise; which R home supplied base R only moved where the collections fell. A crash
  therefore fails the run on every host until fixed, however rarely it reproduces.
- The collector traces every running activation's parked unwind (`Interp::held_unwinds`,
  `a_parked_return_value_survives_its_exit` in `crates/jit/tests/test_tiers.rs`).

## The oracle refuses a partial run and finds the corpus library itself

**Status:** built.

- `oracle compare` refuses to start when a selected case needs a missing package; `--allow-skips`
  makes a partial run whose summary lines start with `PARTIAL`. Each summary counts passed, failed
  (known and new) and skipped. Why refuse up front: the missing packages are known before any case
  runs, and a full pass takes up to 80 minutes; silent skips once made a branch look clean.
- `library_environment` appends `.corpus/library` (`$RY_CORPUS_DIR/library`, written by
  `tools/corpus/install.R`) after any explicit `R_LIBS` when it exists, so `just corpus` is all a
  container needs; an empty `R_LIBS` turns it off. Pinned without R by
  `compare_fails_on_a_missing_package_unless_skips_are_allowed`, which uses a shell script as the
  engine against recorded outcomes.
- `cargo test -p oracle` still drops cases with missing packages, because CI lacks the corpus; the
  merge gate is `oracle compare`.
