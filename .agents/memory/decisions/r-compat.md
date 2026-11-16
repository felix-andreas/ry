# Decisions: R compatibility

Matching GNU R's observable behavior: the oracle, corpus package tests, and R semantics details.

## Package tests through the oracle

**Status:** built.

`oracle packages` (`crates/oracle/src/packages.rs`, `development/corpus-and-oracle.md`, "Package
tests") runs each corpus package's own `tests/` under GNU R and under ry and reports pass or fail per
package. It is the measure for "the corpus packages run on ry".

- **A file passes when its process exits with status 0, and a package passes when every file
  does**, which is the rule `R CMD check` applies to `tests/`. The `.Rout` files are not diffed,
  because they carry timings, temporary paths and addresses; testthat's summary counts are read
  from them and reported beside the verdict, which shows how far a failing package got.
- **The pass rate counts only packages GNU R passes**, because a package whose tests fail on GNU R
  (missing suggests, network, a display) says nothing about ry.
- **GNU R's results are reused** for the same package version unless `--rerun-gnu-r`, since they
  change only when the corpus does.
- **Each file runs in its own process group under `--timeout`**, killed as a group, because a hung
  test usually leaves a child (callr, processx) behind that would keep the pipe open.
- **Run it against a copied binary** (`ORACLE_RY`), not `target/release/ry`, because a full run is
  long and a rebuild under it changes the engine midway.

Fixes the harness drove are classes, each with an oracle case under `crates/oracle/cases/`:
`deparse.level` naming in `cbind`/`rbind` (read from the closure's `...` promises), the `Summary`
group's `na.rm` reordering, list comparison coercion, `return` outside a function, POSIXlt balancing,
graphical parameters, `file.append` and anonymous `file()` buffers.

## Corpus package tests: compatibility details that are easy to get wrong

**Status:** built.

- **Hashed frames list in GNU R's order.** `ls()`, `as.list(env)` and `mget` over a hashed environment follow GNU R's bucket order, and packages' snapshot tests record that order. The order depends on the frame's whole history, because GNU R's table never shrinks on `rm()` and its chain counter (`HASHPRI`, `env.profile`'s `nchains`) falls only when a bucket empties, so replaying the inserts of the live bindings cannot reproduce it once anything has been removed. A hashed frame therefore keeps GNU R's chained table itself (`heap::env::HashTable`, boxed beside the frame), updated on every insert, removal and compaction: buckets from the PJW hash of the name, an insert prepends and counts one (GNU R's `R_HashSet` counts every insert, since its empty-chain test runs after the scan), and past 0.85 of the size the table grows to `size * 1.2 + 1`, moving buckets in ascending order and recounting occupied buckets. The table is the single source of truth for listing and `env.profile`; lookups still go through the address-keyed index. The global environment is hashed from its creation, as in GNU R. `crates/heap/tests/test_frames.rs` checks it against a plain model.
- **String encodings.** Strings intern by pointer, and `mkchar` marks non-ASCII text UTF-8. `Encoding<-` and `iconv` mark text that is valid UTF-8 as UTF-8 even when asked for "unknown", because the session is UTF-8 and a second pointer for the same text would make `==` fail (desc's `read_dcf` broke on duplicate fields). Latin-1 strings stay a separate pointer, and `==` compares them by text through `values::same_text`; `match()` and `unique()` do not yet (`backlog.md`).
- **A sink's connection is a GC root** (`connections::Table::trace`), because GNU R preserves the connection object while output goes to it, and the collector otherwise finalized and closed it under `capture.output`.
- **S4 dispatch on an unsupplied argument dispatches on NULL**, as GNU R's `GetObject` does, even when the argument has a default (`dispatch::dispatch_object`).
- **Queued finalizers root their weak reference's key, value and finalizer** (`libr::boundary::root_finalizable`), because the collector never traces a weak reference's fields and an earlier finalizer can allocate. `RY_GC_VERIFY=1` checks the write barrier's invariant before every minor collection, which is the quickest way to tell a missing barrier from a missing root.

## Matching GNU R's own tests where its behavior is an artifact

**Status:** built, except the black-box rule and the `smooth.spline` and `loess` notes, which "Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate" supersedes, and the long double `List.sum`, which "`List.sum` adds doubles in order in double precision" supersedes.

GNU R's `tests/*.R` pin several behaviors that come from GNU R's machine or its implementation, not from the language. Each is reproduced from observed output (black-box against Rscript), never from GNU R's source.

- **x87 long double sums.** GNU R accumulates `sum`, `mean`, `rowSums`, `colMeans`, `var` and `cor` in an 80-bit long double. A software model (64-bit significand, round to nearest even, exact sqrt) replaced the compensated-sum stand-ins, because only an exact model agrees bit for bit (`mean(c(1:9, -1e300, 1e300))` is 0). The data.table bridge deliberately keeps plain double sums for GForce shapes. On x86_64 the slice loops (`long_double::add_all`, `add_differences`, `add_products`, behind `sum`, `mean`, `var`, `cov`, `cor` and `List.sum`) run on the x87 unit through inline assembly, which rounds identically because System V leaves x87 precision control at 64 bits; the software model is about 10x slower (13 ns an element) and stays the fallback on other targets and on Windows, whose x64 CRT sets x87 precision to 53 bits, and for values outside the x87 exponent range.
- **`%%` and `%/%`** correct the remainder in extended precision, handle a divisor too large for the quotient, and warn past 2^63, matching GNU R's observed results on huge operands.
- **Invalid UTF-8 bytes** survive text processing as private-use code points (`U+10FF00 + byte`, one per byte) and print as `\xNN`; `useBytes` regex functions match byte by byte. The heap itself keeps a string's exact bytes and its encoding flag (`gp::BYTES`, `LATIN1`, `UTF8`), so the stand-ins exist only while `interp` handles a string as a Rust `str`. Carrying bytes through every string builtin instead would touch about 230 `char_str`/`string_at` readers, for no observable gain, because the range is the tail of the last private-use plane: a genuine character there is the one thing the scheme misreads (it becomes a byte when text holding it is interned again). The earlier range, `U+F780..U+F7FF`, sat where symbol fonts map glyphs, so it moved.
- **A string declared as bytes is refused where GNU R translates it.** `values::refuse_bytes` raises "translating strings with \"bytes\" encoding is not allowed" for a non-ASCII bytes string in the builtins whose GNU R code calls `translateChar`: case changes, `chartr`, `sprintf`, `make.names`, `abbreviate`, string `sort`/`order` with more than one element, and `as.vector(mode = "symbol")`. `nchar` raises its own "not computable in \"bytes\" encoding" messages. Functions GNU R runs on the bytes (`paste`, `substr`, `strsplit`, `format`, `useBytes` regex) keep working (`oracle/cases/strings__bytes_encoding.R`).
- **`agrep` and `adist`** use an approximate-match NFA over edit costs; `adist`'s `counts` traceback prefers the order GNU R reports (observed, not derived).
- **Scripts run as console input**: one top-level expression at a time, with `stdin()` reading the rest of the script, because GNU R's tests read data from their own source that way.
- **Message sinks and abort restarts**: `sink(type = "message")` diverts every stderr writer, reaching top level invokes an R-level `abort` restart, and messages are cut to `warning.length`.
- **`.Last.value` holds no reference count**, so binding it never forces a copy on the next modification, as in GNU R.
- **Replacement primitives called directly** (`` `names<-`(x, v) `` outside an assignment) copy their target, because only a complex assignment owns it.
- **`pretty()` and plot axes near the double range**: the 4% axis extension is clamped to ±`DBL_MAX`; a log axis stays inside `log10(0.99*DBL_MAX)` and `log10(1.01*DBL_MIN)`; a zero log limit becomes -320 with GNU R's "nonfinite axis limits -- corrected now" warning. `GEPretty` and `Rf_CreateAtVector` in libR reuse the interpreter's `linear_axis` and `at_vector`, so grid and base graphics share one tick algorithm.
- **Deparse parenthesizes dangling operands**: an `if`, loop or low-precedence unary operator that ends a binary operator's left operand gets parentheses (GNU R's PR#18232 fix). GNU R leaves `function` bare there, which reparses differently; ry keeps the parentheses on purpose.
- **`smooth.spline`** (`rbart`, `bvalus` in `crates/stats/src/spline.rs`) is written from the penalized B-spline method. It copies GNU R's 0.333 in the roughness integral because the printed matrices and fitted values depend on it. Near-interpolating GCV fits can settle on a different `spar`, since the criterion there is rounding noise; that divergence is accepted.
- **`Fexact`, `parseLatex`, `gzcon`, `OutDec`, classed `parseError` conditions and `file(encoding=)` re-encoding** are native implementations of the documented interfaces, checked against GNU R's tests.
- **Bytes-encoded strings force byte matching**: when any pattern or text element of `regexpr`, `gregexpr`, `regexec`, `grep`, `sub`, `gsub` or `strsplit` is marked `"bytes"`, the call behaves as `useBytes = TRUE` (byte positions, `index.type = "bytes"`, non-ASCII results marked `"bytes"`), as GNU R does.
- **TRE compile errors** reject PCRE-only groups (`(?<`, `(?=`, `(?!`, `(?>`, `(?P<`) with "Invalid regexp" and an inverted interval with "Invalid contents of {}". The warning and error wording follows GNU R's three observed paths (wide-character `regexpr`/`grep`, `regexec`, everything else).
- **Accepted divergences**: `fisher.test` on tables whose network exceeds GNU R's FEXACT hash computes the p-value instead of erroring "hash key ... > INT_MAX"; `gsub("\\b", ...)` does not reproduce TRE's re-match of `\b` after an empty match; printing a bytes string pads by its escaped width, not GNU R's single-escaped width.
- **`nlminb` control ranges** are PORT's own `DPARCK` checks: an out-of-range `v` slot sets `iv[1]` to the last failing position, so `nlminb`'s R code prints GNU R's "is out of range" message. `nls(algorithm = "port")` keeps ry's simplified range check.
- **`qbeta` polishes its last digits** with a neighbour walk over adjacent doubles that minimizes `|pbeta(x) - p|`, because GNU R's tests require a mean relative round-trip error below 8e-16.
- **`traceOnOff`** keeps a real tracing state (default on), so `trace()` tracers run.
- **`qbeta` past the log-scale solve**: Newton steps on the caller's own probability scale recover the digits a log-scale solve loses when `|log p|` is large; a NaN from pbeta's asymptotic expansion bisects toward 0; pbeta warnings raised while searching are dropped; a subnormal result snaps to 0 or 2^-1024, whichever probability is nearer the target, because GNU R never returns a subnormal quantile and `d-p-q-r-tst-2` relies on it.
- **`dpois` deviance in double-double**: `ebd0` in `rmath.rs` computes `x log(x/np) + np - x` as an unevaluated sum, with its own atanh-series logarithm, because GNU R's tests pin densities like `dpois(1400, 1000)` to a few ulps.
- **Discrete quantile slack**: a probability within 64 epsilon of the target, measured from the nearer end of [0, 1], counts as reached (16 epsilon of the log probability on the log scale). A slack relative to `p` alone returned the quantile below near 1, and a narrower one exposed `ppois` digits that differ from GNU R's.
- **Deparse of braced `if` in calls**: a braced `if` stays on one line as an argument of a primitive call (`c`, `list`, `quote`, `return`, ...) and breaks inside closure calls, `[` and list elements; `else` after a braced consequent starts its own line.
- **S4 arguments to `cbind` and `rbind`** hand the call to `methods:::cbind`/`methods:::rbind` with `deparse.level` tagged; a deparse level of -1 marks a call from methods' own `cbind2`/`rbind2` defaults and binds directly.
- **`LC_CTYPE` is C.UTF-8** at session start, so `iconv(..., "ASCII//TRANSLIT")` transliterates as GNU R does in a UTF-8 locale.
- **`qnchisq` stops at a relative accuracy of 1e-13**, as GNU R's results do. A full-precision search round-trips some quantiles exactly, and `all.equal` leaves exact matches out of its mean, so `d-p-q-r-tests` line 188 failed on a more precise answer.
- **Accepted numeric divergence**: `pf(x, 1, 1, ncp = 20, lower = FALSE, log = TRUE)` is -17.1486, which an independent mixture sum confirms, while GNU R's less precise noncentral beta gives -17.1318 and `d-p-q-r-tst-2` line 302 asserts GNU R's range.
- **Native `loess`** (`crates/stats/src/loess.rs`) is written from the documented method and black-box checks against GNU R. Fits, robustness weights and `predict` match GNU R; the kd-tree vertex interpolation is a cubic Hermite blend along edges and, in two dimensions, a Coons patch (edge curves plus cross-edge slopes, minus the corner tensor product), found by fitting candidates to GNU R's predictions. Three or more predictors use the tensor product alone, unverified. With `span > 1` the neighbourhood radius is the largest distance times `sqrt(span)`. The approximate statistics (`enp`, `one.delta`, `two.delta`) are computed exactly rather than with GNU R's interpolation table, so they differ in the third digit; above 1000 observations the deltas use a cheaper approximation.
- **URL transfers use `reqwest`, not libcurl** (`crates/services/src/http.rs`). The blocking client was already linked through polars, honors the proxy variables and reads the platform's roots (`SSL_CERT_FILE`). A `url()` connection fetches its whole body when it opens and then reads from memory, like `unz()`. Failures are reported with libcurl's codes and wording, because R code and GNU R's tests match on them. Each request runs on a scoped thread (`on_transfer_thread`), since the blocking client panics inside an async runtime an embedder may own; a transfer thread that dies reports libcurl's "Failed initialization" instead of unwinding into R. `curlGetHeaders` follows redirects hop by hop, so it reports each hop's headers, but not the proxy's CONNECT response that libcurl adds. `libcurlVersion()` reports 8.5.0, the libcurl release whose wording and behavior the transfers follow, so version checks in packages pass; its attributes say what ry really uses (`ssl_version` "rustls", no `libssh_version`, protocols file, http and https).
- **Socket connections are `std::net` streams in blocking mode** (`Device::Socket` and `Device::ServerSocket` in `crates/interp/src/connections.rs`). A non-blocking R connection does not switch the OS socket to non-blocking mode: its read first polls with a zero timeout and, with nothing to read, marks the connection incomplete and returns no bytes, which is how `readLines` holds back a partial line. A blocking read waits up to `socketTimeout()` seconds through the stream's read timeout. Each read refills a 64 KiB buffer with one receive, so `socketSelect()` counts buffered input as ready without asking the OS. Polling is `platform::poll_sockets` (`poll(2)`, `WSAPoll` on Windows). Messages follow GNU R's, with two deliberate differences: `serverSocket(70000)` is an "invalid 'port' argument" where GNU R silently truncates the port to 16 bits, and `socketAccept(timeout =)` gives up after the timeout where GNU R waits forever. With these, `parallel::makeCluster()` works (oracle case `parallel__socket_cluster`). Workers start from `R.home("bin")/Rscript`, so under ry's own R home (`just ry-home`) they run on ry, while under plain `ry run`, whose `R.home()` is GNU R's, they are GNU R processes speaking the same serialization.
- **Unserializing from a connection reads one object** (`unserialize_connection` in `crates/interp/src/serialize.rs`), so several objects on one connection, or a peer's next message on a socket, stay unread. An ASCII string reads exactly its length, which leaves the newline after it unread as GNU R does; an ASCII object that ends in a string is therefore followed by that newline, which the next ASCII header (`\nA\n`) accepts and a binary one does not.
- **`warning(immediate. = TRUE)` signals like any warning**; a flag on the interpreter makes only the default handler print it at once, so calling handlers and `assertWarning` still see it.
- **`Cstack_info()`** reports a fixed size of 7969177 bytes, current use from the remaining native stack, and the evaluation depth from the frame stack, since scripts only compare these against each other.
- **`sort.list` is never an S3 method**: dispatch skips base's own `sort.list` closure when looking for a method of `sort`, because GNU R treats it as a plain function and `sort(x)` on a classed `x` must not reach it.
- **A generic called without its first argument dispatches as on NULL**, matching GNU R's `UseMethod` instead of erroring on the missing argument.
- **An S4 value assigned into a non-list vector** goes through `as.vector` (which dispatches) before `[<-`; into a list it is embedded with GNU R's "implicit list embedding of S4 objects is deprecated" warning.
- **`zeroin2` reports a precision after hitting the iteration limit**: the bracket width `|c - b|`, not -1, so `uniroot`'s `estim.prec` matches GNU R on non-converged searches.
- **JIT function speculation skips frame-bound symbols**: a call of a symbol the frame can bind (a formal or a local assignment) is never speculated from the enclosure, because a formal holding a function shadows a global of the same name (`uniroot(f, ...)` with a global `f`). Pinned by `formal_shadows_enclosing_function` in `crates/jit/tests/test_tiers.rs`.
- **One implementation where two were written in parallel.** `sum` over doubles totals each argument in the long double model, rounds it to a double, and adds it to a long double total, which is what GNU R's observed results across arguments need; main's compensated sum was removed. Parse data comes from `crates/interp/src/parse_data.rs`, which replays GNU R's lexer contexts, instead of a second builder in `lower.rs`. A hashed frame's chains live in one place (`HashTable` in `crates/heap/src/env.rs`), which both `env.profile` and the listing order read. Graphics devices keep the graphics engine's implementations.
- **Impact:** no effect on static analysis. Each behavior has one implementation in `crates/interp`, and the GNU test runner (`development/corpus-and-oracle.md`) is the regression check.

## Runtime semantics settled by the corpus package tests

**Status:** built.

- **A loop accepts a jump from another call only when GNU R's compiler would give it a context**
  (`eval.rs`): its body calls `eval`, `evalq` or `source`, or writes `break` or `next` inside an
  ordinary call. Previously any `break` evaluated in the loop's environment ended it, so
  `h <- function() eval(quote(break), parent.frame())` broke a caller's loop where GNU R reports
  "no loop for break/next". Each loop records its call depth and whether it has a context, and
  compiled closures apply the same test. Pinned by `base__loop_jumps_from_other_calls`.
- **One implementation per attribute setter.** libr's `Rf_dimnamesgets` and the `row.names` arm of
  `Rf_setAttrib` call the interpreter's `set_dimnames` and `set_row_names` (`subset.rs`). The libr
  copies had drifted: libr's `row_names_gets` missed the double `c(NA, -n)` form, so readr's
  chunked reader saw two-row frames forever.
- **A user interrupt is a condition first.** `Rf_onintr`, `R_CheckUserInterrupt` and the
  evaluator's interrupt poll call `conditions::signal_interrupt`, which offers an
  `interrupt` condition to calling and exiting handlers and unwinds to top level only when none
  takes it, as GNU R's `onintr` does. Previously `Unwind::Interrupt` bypassed the handler stack,
  so `tryCatch(rlang::interrupt(), interrupt = )` halted the script (later's tests).
- **A pending interrupt waits for the next interrupt check** (`interp::unwind`).
  - *Previous shape.* `R_interrupts_pending` and `R_interrupts_suspended` were libr globals the
    interpreter never read, `.Internal(interruptsSuspended())` always answered `FALSE` and set
    nothing, and `INTERRUPT_REQUESTED` alone recorded a Ctrl-C, which the very next call raised.
    So `suspendInterrupts(rlang::interrupt())` fired as soon as `rlang::interrupt()` ran.
  - *Shape.* The interpreter owns both globals, exported under GNU R's names
    (`INTERRUPTS_PENDING`, `INTERRUPTS_SUSPENDED`), so C code, `suspendInterrupts()` and the
    evaluator share one record. `INTERRUPT_REQUESTED` only says "attend at the next poll", and stays
    raised while an interrupt is pending. The evaluator's poll (calls, loop iterations, compiled back
    edges through deoptimization) delivers a pending interrupt on every 1000th poll, as GNU R's
    `eval` checks every 1000 evaluations; `R_CheckUserInterrupt` (`check_user_interrupt`) delivers
    at once unless suspended; `Sys.sleep()` and socket waits deliver even while suspended, because
    GNU R waits in `R_SelectEx`, which lifts the suspension. Leaving `suspendInterrupts()` delivers
    nothing by itself. A C context saves and restores the suspension, as GNU R's contexts do, and
    `ry run` installs the SIGINT handler, as `Rscript` does.
  - *Known gap.* A host that sets `R_interrupts_pending` directly, without the signal handler, is
    seen only by `R_CheckUserInterrupt`, not by the evaluator's poll, which reads the attention flag.
  - Pinned by `crates/libr/tests/test_interrupts.rs`, which runs in its own process because it loads
    base R, and the oracle cases `conditions__suspended_interrupts` and
    `conditions__interrupt_handlers`.
- **ALTREP vectors are first-class at GNU R's thresholds.** Every `1:n`, `seq_len` and
  `seq_along` of two or more elements is a compact sequence (`sequence` in
  `crates/interp/src/arith.rs`, through the `compact_sequence` host hook), `as.numeric` of one
  stays compact, `as.character` of any number vector without attributes is a deferred string, and
  `duplicate` of a vector of 64 or more elements that changes only attributes (`unclass`,
  `names<-`, `attr<-`, `structure`, `dimnames<-`) makes a wrapper over the shared data
  (`values::shallow_duplicate_attr`, `WRAP_THRESHOLD`). `.Internal(inspect())` and data.table's
  ALTREP checks (754.07, .08, .10) therefore see what GNU R produces. The earlier thresholds
  (2^20 for sequences, 256 for deferred strings, no wrappers) existed because tier 0 and the JIT
  read plain data in place; the placement rule below removes that cost instead.
  - *Placement rule.* A plain read pays one ALTREP bit test and nothing else; every ALTREP branch
    is `#[cold] #[inline(never)]` and out of line. Tier 0's in-handler fast paths must not run R
    code or collect, which a package class's `Elt` method may do, so tier 0 reads only compact
    sequences in the handler (pure arithmetic via `values::compact_sequence`,
    `read_altrep_element_call` in `crates/interp/src/bytecode.rs`). Any other ALTREP read
    declines to the `[`/`[[` builtin, whose `element_access::altrep_element` reads one element
    through the class's `Elt` method. The JIT reads attribute-free compact integer and double
    sequences inline: the `INTEGER_SEQUENCE` and `DOUBLE_SEQUENCE` feedback bits type such a value
    as `Ty::IntSeq` or `Ty::DblSeq`, whose guard tests the ALTREP flag, the class pointer and nil
    attributes, and a `for` loop, `x[i]`, `x[[i]]` or `length(x)` on it computes from the class's
    `c(n, n1, inc)` info vector (`sequence_info` in `crates/jit/src/codegen.rs`). The double form
    matters because `as.double(1:n)` stays compact; without it `x[i]` ran as a boxed builtin call
    at 1,273 Ir per iteration. Feedback words are u32 because the two sequence kinds and the
    string kinds do not fit in 16 bits. Compiled code embeds the class pointer, so it is valid
    only in the process that compiled it. Every other ALTREP value side exits on `OBJECT|ALTREP`.
  - *Sequence speculation never mixes with a plain vector.* `a:b` with `a == b` is a plain
    scalar, so the `IntSeq`/`DblSeq` guard also accepts a non-ALTREP length-one vector of the
    same kind, and feedback that saw only that scalar at a `:`, `seq_len` or `seq_along` site
    still types the result as a sequence. A sequence joined with a plain vector is `Boxed`, never
    `IntVec`: the slot join keeps a sequence a path forgot (`inds <- 1:n` on one path, `inds <-
    which(...)` on the other), and a partly loaded sequence variable joins with its load
    feedback. Load feedback that saw only one kind (sequence, scalar or vector of integers, or of
    doubles) types the variable as the plain vector, so `perm1 <- 0:(n-1)` followed by element
    stores stays `IntVec`. A variable the body assigns into (`x[i] <- v`, `x[[i]] <- v`, a
    replacement call) is never a sequence: `SetVar` of a sequence into it stores an expanded copy
    (`jit_sequence_contents`, a side exit when the value is some other ALTREP), and a load that
    still finds a compact value copies it and marks the slot as an outer variable, so the first
    element store copies again and writes the result back instead of losing the store in a
    temporary. Converting on a join edge was rejected: an edge has no side exit and the copy
    would need its own reference ownership. `for` normalizes its
    sequence register to the plain vector type, so a loop header that joins a compact pass with a
    materialized one stays typed and the loop step branches on the info vector. Without these,
    fannkuchredux and fasta_3 deoptimized and recompiled their main body up to four times
    (`jit__sequence_speculation` pins the shapes).
  - *Reads leave a sequence compact.* GNU R's `print`, `[`, `[[` and `for` read through `*_ELT`,
    so a compact sequence stays compact after them; only a data-pointer read (`format`, most
    arithmetic) expands it. ry matches: printing reads an unexpanded sequence into a fresh vector
    (`values::read_contents`), `[` on a compact sequence always takes the element path and
    computes each element, `[[` reads numbers through `Elt`, and the tree-walking `for` (top-level
    code) reads numbers element by element, which keeps `for (i in 1:1e9)` at top level at 43 MB
    instead of 3.9 GB. `altrep__reads_keep_compact` pins the `inspect` state.
  - *Arithmetic reads a sequence unexpanded.* The vector kernels (`crates/interp/src/kernels.rs`)
    read a compact sequence operand as a `Sequence` reader that computes each element from its
    first element and increment, so `flows * exp(-rate * seq_len(n))` pays only the sequence's
    creation and allocates no expansion. Only C code asking for `DATAPTR`/`INTEGER_RO` expands.
    This deliberately differs from GNU R, whose arithmetic reads through `INTEGER_RO`/`REAL_RO`
    and so leaves the operand `(expanded)` in `inspect`. The values are the same
    (`altrep__sequence_arithmetic`), and keeping the operand compact saves the expansion's memory,
    so no oracle case pins the inspect state after arithmetic. `abs` and the accuracy check of
    `%%` still expand through `values::ints`/`arith::real_at`.
  - *Summaries ask the class, then read without expanding.* `sum`, `min` and `max` of a lone
    integer or double ALTREP argument call its class's `Sum`, `Min` or `Max` method first (the
    `altrep_summary` host hook), as GNU R does, so `sum(1:n)` reads no elements. When the class
    has no answer, a compact sequence is still not expanded, because GNU R reads it by region:
    `min` and `max` read its two ends, and `sum`, `prod`, `all`, `any` and `mean` read it into a
    fresh rooted vector (`SequenceRead` in `crates/interp/src/arith.rs`). `range` expands it,
    since GNU R's `range.default` combines its arguments with `c()`. Each case was checked against
    GNU R's `inspect` one process at a time, because Rscript compiles a top-level loop and folds a
    `3:12` in its body into one shared constant, which pollutes a looped comparison.
  - *Assignment unwraps.* Storing a wrapper into a variable or slot keeps its data when the
    wrapper is the only owner (`values::unwrap_assigned`), so a hot loop sees a plain vector.
  - *Wrappers share data as GNU R's do.* A shallow duplicate of a wrapper shares the wrapped
    data; only a deep copy copies it. Every ALTREP instance is marked not mutable (refcount
    65535 in `new_altrep`), so the interpreter copies before any write, and a write through the
    data pointer of a shared wrapper copies first.
  - *Materialize keeps attributes and skips attributed slots.* A wrapper's data keeps the
    attributes it had when wrapped (GNU R's `DUPLICATE_ATTRIBUTES`), so `materialize_contents`
    returns a stored slot for the object only when that slot has no attributes. Otherwise xts's
    `unclass(x) - 1` dispatched `-.yearmon` on the unclassed result.
  - *`duplicate` returns fresh storage.* `memory::duplicate` of an ALTREP object without a
    `Duplicate` method materializes it and copies again when the result is storage the object
    keeps (a compact sequence's expansion), because C code writes into duplicates: data.table's
    `frev` and `setkey` reversed or sorted the source `1:n` otherwise.
  - *Sparse reads.* `x[[i]]` and a sparse `x[i]` read through `values::int_elt`/`real_elt`; `x[i]`
    on another ALTREP number vector reads element by element when the vector has 2^16 or more
    elements and the index selects at most an eighth of them (`reads_sparsely`,
    `crates/interp/src/subset.rs`), and expands once otherwise. `base__long_sequences` keeps `seq_len(2^31 - 1)` under 1 GB peak.
  - *Representation differences that remain.* `inspect` prints the refcount of a wrapper or
    deferred string as `REF(65535)` (ry's not-mutable mark; GNU R prints the real count) and a
    compact sequence a data-pointer read expanded as `(expanded)`, as GNU R does. Lists have a `wrap_list` class, as in GNU R
    4.6.
  - *Cost.* A plain read costs no more than before (measured with callgrind on a tier-0 loop
    `acc <- acc + x[i]`). A compact read in tier 0 costs about 110 to 190 Ir more per iteration than a
    plain one, against about 1,000 to 1,900 more when it declined to the builtin. Tier 0 reads the
    class and the `c(n, n1, inc)` state inline (`values::compact_sequence`, with the class
    identities in `Interp::compact_sequence_classes`), not through a host call. A deferred string
    read converts only that element through `values::altrep_string_elt`, gated on
    `Interp::deferred_string_class` because that class runs no R code; other ALTREP strings decline
    to the builtin, since tier 0 must not re-enter R mid-instruction.
  - *Follow-up.* The compact read in tier 0 is not yet within 20 Ir of a plain one, and the JIT
    side exits on deferred strings, wrappers and package classes until an extern element reader
    (`ry_altrep_int_elt` and the real and string forms) lands behind its `OBJECT|ALTREP` guard. Arithmetic on a wrapper over attributed data materializes and therefore
    copies, where GNU R reads through the data pointer.
  - *The JIT reads a sequence's info from the object on every read.* `sequence_info` in
    `crates/jit/src/codegen.rs` loads the info pointer and `(count, first, step)` from the
    instance, with the scalar path of the representation check marked cold. Per-slot SSA
    variables for the info were measured and rejected: slot status can be not-loaded at a loop
    head (on-stack-replacement entry), so the variables become block parameters that cost more
    than the reload. Speculating the representation in the slot guard and folding the bound check
    into the index compare saved nothing measurable either. Within a chain of fall-through blocks
    (`Plan::continues` in `crates/jit/src/analysis.rs`, a block whose only predecessor is the
    fall-through from the block before it and which is not a loop head) `known_object` reuses a
    sequence read already checked. The floor: a compact read is a flag test and branch, the info
    pointer, three loads, a conversion, a multiply and an add, about 8 to 10 instructions more
    than a plain load, and Cranelift does not hoist loop-invariant work. Convolution
    (two reads per 157-instruction iteration) therefore runs at about 1.06 to 1.08x main's Ir and
    nbody at about 1.01x. Expanding a compact sequence on its first compiled element read would
    close the gap but diverges from GNU R, which keeps a sequence `(compact)` in `inspect` after a
    compiled `a[i]` loop.
  - *Only the user-visible `as.double` keeps a sequence compact.* The compact-preserving branch
    sits in `as_type` (`crates/interp/src/coerce.rs`), not in `coerce_vector_quiet`. A builtin's
    own coercion reads the elements immediately, and an expansion on that read allocates while the
    builtin holds unrooted results: `complex(real = 1:2, imaginary = 3:4)` crashed under GC stress
    when the branch sat in the shared coercion.
  - *A new compact sequence is not rooted.* The interpreter makes one for every `a:b` it
    evaluates, and a handle per sequence would pin each one until the enclosing call returned.
    `new_sequence` (`crates/libr/src/altrep.rs`) relies on the ALTREP allocation keeping its info
    vector alive, and a native caller roots the result on its way out of `boundary::enter`.
  - *`a:b` compiles without the builtin call.* `Intrinsic::Colon` calls `jit_colon`, which
    builds the integer result through `interp::arith::colon` and returns NULL for doubles, a
    missing or out-of-range bound, or a heap error (parked), so tier 0 makes those results and
    raises those errors. A site whose feedback shows doubles keeps the builtin call. The tier 0
    builtin call cost about 1,250 Ir per `a:b`, more than the compact sequence's whole creation,
    which is why the sequence's second allocation was not folded into the instance.
  - Pinned by the oracle cases `altrep__wrappers` and `datatable__compact_columns` and by data.table's
    own suite.

## Package-test compatibility details that look optional but are not

**Status:** built.

- **Warnings always go through `.signalSimpleWarning`.** GNU R's `vsignalWarning` calls `.signalSimpleWarning(msg, base::quote(call))` whenever that function is bound, even with no handler established, and rlang reads the frames it leaves. ry used to call it only when a handler existed (`crates/interp/src/conditions.rs`, `warning_call`).
- **Compiled closure calls pass constant arguments as values.** GNU R's byte code pushes a constant argument with `PUSHCONSTARG`, without a promise, and rlang's `env_dot_type` tells the two apart. `compiler::cmpfun` compiles at once, as GNU R's does (`crates/interp/src/bytecode.rs`, `bcClose`; `eval::compiled_args_into`).
- **`dyn.load` refuses GNU R's `tcltk.so`.** GNU R builds its base packages' shared objects with R's internals, so they read `SEXPREC` fields directly and crash on ry's heap layout; the other base packages' code is built into libR (`builtin_libraries.rs`), and tcltk is refused with "Tcl/Tk support is not available on this system", which sessioninfo's `external_info()` catches. `capabilities("tcltk")` is `FALSE` to match.
- **Parse errors use GNU R's exact text.** testthat and other packages match on it ("unexpected symbol", the quoted line). The message names the token GNU R's LALR parser stops on (`SyntaxError::unexpected`) with bison's token names, the location is that token's 1-based column with tabs to multiples of 8 (end of input is column 0 on the line after the last), and the context is the last 255 bytes read, split into lines, the last two quoted with a caret. `Rscript` and the console quote only the failing top-level expression, `Error: ... in "..."`, and nothing at the end of input (`lower.rs`, `grammar_error_report`; oracle case `parse__error_messages`).
- **`list.dirs` names the directory itself only when `recursive = TRUE`.** testthat reads snapshot variants with `list.dirs(recursive = FALSE, full.names = FALSE)`, and a stray `""` entry became a phantom variant.
- **Base binds the missing argument in the C API.** `R_MissingArg` is a symbol whose base value is itself, so `R_existsVarInFrame`, `R_GetBindingType`, `R_BindingIsActive` and `R_BindingIsLocked` answer for it in the base frames instead of erroring (`crates/libr/src/environments.rs`, `base_binds_missing_arg`).
- **A compiled `local(expr)` is `(function() expr)()` with a four-element `function` call**, whose srcref slot is NULL, as GNU R's compiler writes it. rlang's `trace_back` writes into that slot of every frame call.
- **`stdout()` is the current sink target.** GNU R's `stdout()` returns `R_OutputCon`, so a call inside `sink()` names the sink, while a `stdout()` kept from before writes past it to the console (connection 1 is the console itself, never redirected). vctrs' reporter tests broke on a stray `.` the reporter wrote through a saved `stdout()` (`crates/interp/src/connections.rs`, `stdout`, `device_write`, `print`).
- **A connection's `conn_id` address is never compared.** GNU R only checks that the external pointer is not NULL (closing clears it); curl stores its own handle there, so comparing addresses made `isOpen` and `close` reject curl connections (`connections.rs`, `resolve_number`).
- **TRE's lazy quantifiers with capture groups take the leftmost-first match.** Without groups TRE returns the shortest overall end; with groups its tagged matcher follows the leftmost-first rule (`x(.+?)(a+)` on `xaaab` is `xaaa`, `x.+?a+` is `xaa`). openssl's PEM splitting depends on it (`crates/interp/src/strings.rs`, `Matcher::find_at`). `\Q...\E` quotes a literal run.
- **`attach(env)` attaches a copy.** GNU R copies the bindings into a fresh environment and leaves the original's enclosure alone; attaching the environment itself cut shiny's namespace off from its imports (`crates/interp/src/envir.rs`, `do_attach`).
- **A user interrupt is signalled as a condition first.** `Rf_onintr` and `R_CheckUserInterrupt` run the handler stack for class `c("interrupt", "condition")`: calling handlers run, an exiting handler (`tryCatch(interrupt = )`) catches it, and only an unhandled interrupt unwinds to top level. later and rlang's `interrupt()` rely on it (`crates/interp/src/conditions.rs`, `signal_interrupt`).
- **`rmultinom` carries the remaining probability as a double-double.** GNU R uses a `long double`; in plain `f64` ten probabilities of 0.1 leave a conditional probability just above 0.5, `rbinom` then draws the complement, and the counts differ from GNU R's for the same seed. RcppArmadillo compares R's draws with its own (`crates/interp/src/distributions.rs`, `rmultinom`).
- **`[` subsets a parsed expression's `srcref` list.** plumber locates each endpoint's comment block with `attr(exprs[i], "srcref")` (`crates/interp/src/subset.rs`, `subset`).
- **`Rf_setAttrib(R_NilValue, name, R_NilValue)` succeeds.** GNU R checks for removal before it rejects a `NULL` target, and data.table's `C_unlock` runs on a `j` result that is often `NULL` (`crates/libr/src/attributes.rs`, `set_attrib`).
- **ALTREP vectors match GNU R's thresholds**: see "ALTREP vectors are first-class at GNU R's thresholds" for the placement rule, wrappers and the remaining `inspect` differences.
- **`row.names<-` stores a real compact form `c(NA, n)` as integers**, as GNU R's `row_names_gets` does. cpp11's `data_frame::nrow` reads only the integer form, and readr's chunked reader loops forever on the real one.
- **`R CMD INSTALL` of a directory leaves the shared objects in its `src/`.** The build runs in a copy outside the source (so a Cargo workspace never adopts it), but `pkgbuild::compile_dll` and `pkgload::load_all` expect GNU R's in-place result, so the objects are copied back (`crates/rcmd/src/rcmd.rs`, `install_source`).

## Runtime error checks follow GNU R's order and blame

**Status:** built.

- **Rule.** Where GNU R checks a condition before matching arguments or dispatching, ry checks it at the same point and blames the same call. `NextMethod()` errors through the innermost closure call ("called from outside a function", "called from an anonymous function", "object not specified" when `.Class` is unbound and the object has no class, "generic function not specified", "no method to invoke"), with no fallback to the call's name. `round()` and `signif()` count their arguments before matching them and blame the calling closure, so `eval.rs` leaves their formals check to them.
- **Why.** Packages and GNU R's own tests compare these messages, and a lenient fallback hid real bugs (a `NextMethod` that silently picked the wrong generic).

## A formula's dot expands to the sum GNU R prints

**Status:** built.

- `stats::models::expand_frame_dot` replaces `.` in a formula's right-hand side with the sum of the frame's other columns only when the dot is reached through formula operators. The sum is wrapped in parentheses unless the dot stands at the top, inside `(`, as a call argument, or as the left operand of `+`, which reproduces GNU R's `terms()` output and deparse exactly.

## The corpus's waiting is the suites' own sleeps, and child R processes run ry

**Status:** built.

- callr and processx start `R.home()`'s `bin/R` or `bin/Rscript`. In ry's R home these are scripts that exec `bin/exec/ry-rcmd`, so children run ry's runtime (`exists(".ry_view", baseenv())` is `TRUE` in a child). `Rscript -e` starts in about 32 ms against GNU R's 140 ms, and one `callr::r()` round trip takes 0.17 s against 0.25 s.
- In the main process of the corpus suites, waiting is `Sys.sleep` called by the tests themselves (foreach 77% of its run, lgr 22%, cachem 10%), which costs GNU R the same. rlang and purrr set `Config/testthat/parallel: true`, so testthat runs their tests in one callr worker even with `TESTTHAT_CPUS=1`, and the main process sits in processx's `poll` waiting for it: that is where a 90% waiting share for them came from. The worker is a ry process doing the real work, so this time speeds up with ry; profile such suites with `RY_PROFILE_SUMMARY_DIR` (one summary per process) and read the worker's.

## The package corpus runs offline

**Status:** built.

- **Previous shape.** `just bench-packages` let suites reach the network. `jsonlite`'s `test-network-Github.R` has no `skip_on_cran()` and opened 7 api.github.com URLs per run, about 0.2 s each on both engines, so the round trips measured the proxy and GitHub, not ry, and dragged `jsonlite`'s ratio toward 1x (1.26x with them, 1.52x without).
- **Shape.** `NETWORK_TESTS` in `tools/bench/tooling/packages.py` lists each excluded test file with its reason, and `unpack_tests` deletes them. Every engine runs with `http_proxy`, `https_proxy` and `all_proxy` (both cases) set to a local proxy that answers 403 to everything and records the host, with `no_proxy` for loopback. After the table prints, any recorded host fails the run before `--write`, naming the hosts, so a new network test cannot slip in silently. Both GNU R's libcurl and ry's `url()` (reqwest) honour the variables; checked with `readLines(url(...))` for http and https on both.
- **Effect.** The corpus geomean moved from 1.99x (95% interval 1.79x to 2.21x) to 2.01x (1.83x to 2.20x), 21 packages, 3 repeats, same build.

## The oracle suite names the GNU R build it was recorded with

**Status:** built.

- **Previous shape.** Nothing recorded which R wrote an `.expected` file. `record`, `verify` and `RY_BLESS=1` used the R on `PATH`, so a machine with GNU R 4.3.3 there blessed 4.3.3 output into a suite meant to be 4.6.2: eight cases held 4.3.3 output and one a raw dump, and the ry behaviors they pinned were 4.3.3's.
- **Target shape.** `crates/oracle/cases/gnu-r.txt` names the release and the BLAS and LAPACK libraries (file names, so an install prefix does not matter), asked from R itself (`oracle::gnu_r_build`). Another release refuses `verify` and `record` and skips the recorded-outcomes test, because its outcomes are another release's semantics; other libraries only warn, because they move last digits. Every report prints the build it ran. One file per suite, not a header per `.expected`, because a suite is recorded with one build and a per-file header would be 360 copies of one fact.
- **Library-dependent cases.** "Only warn" still compared every case exactly, so `recorded_outcomes_match_gnu_r` failed on any OpenBLAS host (CI's included) on the cases that print BLAS, LAPACK or libm results to the last bit, where an optimizer's counts and warnings move too. Those cases carry a leading `# oracle: library-dependent` line (`oracle::library_dependent`), and the test skips them, by name, when the libraries differ from the build file's; every other case is still compared exactly. libm is not in the build file (glibc's x86-64-v3 build moves `pgamma`, `fft` and `nlm` digits), so its cases are compared only where BLAS and LAPACK match too. A tolerance was rejected because iteration counts and warnings differ, not only digits.
- **Crashes.** A GNU R killed by a signal before writing its report is `EngineError::Crashed`, and the case is skipped with the build named: a hardened 4.6.2 build aborts with "buffer overflow detected" on four graphics cases that the reference source build runs. A crashing second engine is a failure.
- **Impact.** Bless on the wrong R fails loudly instead of rewriting the suite, and a diff between two builds is told apart from a regression. The forced oracle still compares ry with the live GNU R, running ry on that R's base, so on a 4.3.3 host it tests ry's 4.3.3 emulation, not what users run; point `ORACLE_R` at a 4.6.2 build for that.
- Two properties of the machine decide the last digits of numeric output. The first is the C math library: glibc's x86-64-v3 build rounds `hypot` differently from the baseline build on about one argument pair in eight. The second is FMA contraction: a compiler at `-O2` with `-march=x86-64-v3` fuses a multiply and an add, which moves the last digits of `df`, `dnchisq`, `pnchisq`, `qnchisq`, `pgamma`, `fft` and `nlm`.
- The libr test packages therefore compile with `-ffp-contract=off` (`NO_CONTRACTION` in `crates/libr/tests/harness.rs`). `crates/libr/tests/test_numerics.rs` fingerprints `hypot`, and the header of `rmath_expected.tsv` carries the same fingerprint, which `expectations_are_current` in `test_rmath.rs` compares with the running library. An oracle case that prints such digits carries `# oracle: library-dependent`.
- To reproduce the v3 library on a baseline machine, unpack Ubuntu's amd64v3 `libc6` package and point a test binary at it with `patchelf`.
- A portable oracle case avoids three machine differences. C code built with `R CMD SHLIB` includes every header it uses, because newer compilers refuse an implicit declaration. libcurl 8.8 and later drops a bare trailing `?` from a URL, so a case that needs an empty query sends `?=`. Docker's default seccomp profile makes `system()` attach "Function not implemented" to its result, so a case does not print that attribute.

## The oracle gives every engine the reference R's library paths

**Status:** built.

- **Previous shape.** Each engine searched its own default libraries, and every track set `R_LIBS` by hand to make them agree. ry's defaults are its bundled base R's library, which holds none of the reference R's packages, plus any GNU R user and site libraries it finds (`read_renviron` in `crates/interp/src/session.rs`). In a container with only `/opt/R/4.6.2`, `--engine ry` found no R6, rlang or data.table and failed 16 cases. With Debian's `/usr/lib/R/site-library` present, ry loaded its data.table 1.14.10, rlang 1.1.3 and withr 2.5.0 instead of the reference R's, and the same cases failed with other diffs.
- **Target shape.** `library_environment` in `crates/oracle/src/oracle.rs` asks the reference R for `.libPaths()` once, with `R_LIBS_SITE` and `R_LIBS_USER` empty and the caller's `R_LIBS` kept, and every engine run (cases, gallery, `oracle bench`, and the `installed_packages` probe that decides which cases can run) gets `R_LIBS` set to that list and both other variables set empty. ry's runtime adds GNU R's site and user libraries only to a variable that is unset, so empty values turn them off without touching runtime semantics. `just doctor` compares the packages `ry run` would load by hand (first match over its `.libPaths()`) with the reference R's and prints the three variables when they differ.
- **Why the harness and not ry's defaults.** Searching other GNU R installations' libraries is how an installed ry finds the packages a user already has (`reference/cli.md`, "How ry finds R"), so it stays. The oracle's job is a comparison with one fixed R, so it pins the libraries to that R's.

## Known oracle failures live in a committed file

**Status:** built.

- **Previous shape.** Each track learned which oracle failures main already had from messages between sessions, so a branch could not tell an inherited failure from its own regression without rerunning main, and a fixed case stayed "known" in people's heads.
- **Target shape.** `crates/oracle/known_failures.toml` lists each case and engine main fails, with the area of ry the bug lives in and a one-line reason (`provisional` while it is seen only on a branch). `oracle compare` (which takes a comma-separated engine list and prints one summary line per engine) and `second_engine_matches_gnu_r` report a listed failure as known and fail on an unlisted failure or on a listed case that passes, so the fix and the entry's deletion land together. The oracle refuses an entry with an unknown engine, a missing case or a duplicate. `ry-jit-gctorture` (`RY_JIT=force` plus `RY_GC_STRESS=1`) is a named engine because compiled code that drops a root shows up only with both on.

## A crash is never a known failure

**Status:** built.

- **Previous source of truth.** `intermittent = true` on an entry in `crates/oracle/known_failures.toml` let a listed case under a gctorture engine pass on some hosts without failing the run, and a listed case that crashed counted as known like any other failure. Two segfaults under `ry-jit-gctorture` carried the mark: `print__deparse_undefined_s4_class`, which crashed when the engine loaded base R from GNU R's home, and `s4__native_definitions`, which crashed with ry's own home.
- **What was weak.** The mark recorded a memory-safety bug as host noise. The fix for the crashes traced both to one cause: a compiled body hands tier 0 an unwind by parking it in its activation, and the collector did not trace activations, so the value a `return()` carried was freed by a collection during the deoptimization exit that followed. Which home supplied base R only moved where the collections fell.
- **Target shape.** The collector traces every running activation's parked unwind (`Interp::held_unwinds`, pinned by `a_parked_return_value_survives_its_exit` in `crates/jit/tests/test_tiers.rs`). `KnownFailure` has no `intermittent` field, and `parse_known_failures` refuses an entry that carries the key. The oracle tells a crash from a failure (`CaseRun` in `crates/oracle/src/oracle.rs`): a case whose engine was killed by a signal prints as `CRASHED` and gets the verdict `New` under every engine, listed or not.
- **Impact.** A crash fails the run on every host until it is fixed, however rarely it reproduces, and no entry can excuse it. A known failure that is not a crash fails the run when the listed case passes, so a fix and its entry's deletion land together.

## The oracle refuses a partial run and finds the corpus library itself

**Status:** built.

- **Previous shape.** A case that needed a package GNU R could not load was skipped, and the summary still read `N of N passed`, counting only the cases that ran. The oracle searched only the reference R's library unless the caller set `R_LIBS`, so in a container whose reference R has no extra packages every corpus-dependent case (22 at the time) skipped silently and a branch looked clean.
- **Target shape.** `oracle compare` refuses to start when any selected case needs a missing package, and `--allow-skips` makes a deliberately partial run whose summary lines start with `PARTIAL`. Each summary line counts passed, failed (known and new) and skipped. `library_environment` puts `.corpus/library` (`$RY_CORPUS_DIR/library`, as `tools/corpus/install.R` writes it) after any explicit `R_LIBS` whenever the directory exists, so `just corpus` is all a container needs; an empty `R_LIBS` turns the corpus off. `compare_fails_on_a_missing_package_unless_skips_are_allowed` pins it without R, using a shell script as the engine against recorded outcomes.
- **Why refuse instead of failing at the end.** The missing packages are known before any case runs, and a full pass takes up to 80 minutes, so a run that would be rejected anyway should stop in seconds.
- **Not changed.** `cargo test -p oracle` still drops cases whose packages are missing, because CI runs it in environments without the corpus; the merge gate is `oracle compare`.
