# Decisions: DataFrame and the table engine

The built-in DataFrame, its kernels, CSV reading, and the data.table boundary.

## The built-in frame

**Status:** built, except the name `frame` and its class, which "The DataFrame API is Polars', and ry's own types are UpperCamelCase" supersedes.

ry's typed table type is spelled `frame`. It is a Polars `DataFrame` held by the `table` crate,
which links Polars as a Rust crate, and the checker types it as `frame{col: type, ...}`.

- **The name is `frame`, not `table`.** Base R's `table()` and its `"table"` class must keep
  working, so the type, the constructor `frame(...)` and the runtime class
  `c("frame", "data.frame")` use a name base R does not give a meaning to in a type position.
  `graphics::frame()` exists, so the checker treats `frame()` as the built-in only with at least one
  argument and when the project defines no `frame` of its own.
- **At run time `frame` is one closure that delegates a call without arguments to
  `graphics::frame`**, and it is bound in an environment attached ahead of `package:graphics` on
  the search path. `graphics::frame` is `plot.new`, which takes no arguments, so every call that
  worked before still runs graphics' code, and every call with arguments, which was an error, now
  builds a frame. R's own lookup then gives the checker's rule for free: a `frame` defined in
  `.GlobalEnv` or a package namespace is found first, and a package that imports `graphics::frame`
  keeps it. The rejected options: binding it in base (top-level code finds `package:graphics` before
  `package:base`, so `frame(a = 1)` would hit `plot.new`), and renaming the type (the name was
  chosen to stay clear of base's `table`). The cost is that `identical(frame, graphics::frame)` is
  `FALSE`, which no known code tests. The interpreter wiring that binds it is still open.
- **`:=` has value semantics in the checker.** `f[, y := e]` rebinds `f` to the extended type, and
  aliases keep the old type. Column sets are exact. At runtime the update is copy-on-write: the
  interpreter passes an unshared `&mut Table` (reference count one) and the update happens in place,
  or it calls `Table::fork` first, which copies only the column list, because Arrow buffers are
  immutable and shared. data.table's own `:=` on a `data.table` keeps its by-reference semantics.
- **`:=` writes only a slot the current scope already owns** (`naming.rs`, the `Index` arm).
  Naming runs before types, so it cannot tell a frame from a `data.table`. Minting a local binding
  for every `NameRef[, y := v]` made a global `data.table` updated inside a function resolve to the
  `:=` statement, which broke goto-definition, references and rename. So a top-level name, a local
  or a parameter is rewritten in place, and any other subject binds nothing. For a global frame the
  checker still records the update as a refinement of the non-local name (`non_local_slots`, the
  mechanism guard narrowing uses), so later reads in that function see the new columns.
- **`:=` into an existing column keeps the column's type unless the value replaces it whole**,
  because data.table coerces the value to the column. The engine (`assigned_type` in `query.rs`)
  keeps the type under an `i`, under a `by`, and for a value that collapses to one row; a full-length
  value with neither replaces the type. A string written into a factor keeps the factor and appends
  the new labels as levels in order of appearance, as data.table does. The checker cannot see that
  `sum(x)` is one value, so it treats only a literal as one (`is_literal` in `check.rs`), because a
  scalar-like type such as `as.character(x)`'s is not a length guarantee. A column the statement
  creates takes the value's type.
- **Queries go through a small IR** (`crates/table/src/query.rs`), not straight from R syntax to
  Polars, so the interpreter, the checker's result types (`result_type`) and the tests share one
  description of `tbl[i, j, by]`. Lowering gives operations R semantics on top of Polars: integer
  `+ - *` overflow to `NA`, a top-level integer `sum` that does not fit becomes a double (as in
  R 4.3 and data.table's gsum), a NaN comparison is `NA`, `max`/`min` return NaN when one is
  present, `!`/`&`/`|` treat a nonzero number as `TRUE`, aggregates propagate `NA` unless
  `na.rm` (a `null_count` guard that costs nothing when the input has no nulls), `%/%` floors, `%%`
  takes the divisor's sign, and `cor` is `NA` where Polars gives `NaN`.
- **data.table's layout is the contract for joins**: rows follow `i`, `x`'s columns come first with
  the join columns carrying `i`'s values, then `i`'s other columns, a clash prefixed `i.`. Join
  columns of different types meet through a common type (two factors as strings), and the join
  column is cast back to `i`'s type.
- **`by` keeps first-appearance order and `keyby` sorts and keys**, matching data.table, through
  `group_by_stable` and a sort.
- **ALTREP column exposure lives in `libr` (`crates/libr/src/frame_altrep.rs`), and `libr` links
  Polars.** Frames are interpreter values, so `interp` depends on `table`, and `libr` (which
  depends on `interp`) links Polars either way; keeping ALTREP in `table` behind a feature would
  only make the engine depend on the runtime. The dependency points from the runtime to the
  engine, and `libR.so` grows by the engine's size. An exposed column's `data1` owns the Arrow
  array and `data2` holds R's layout once something needs it; a column that R never materialized
  comes back from `import_column` as the same buffer.
- **NA is Arrow validity, NaN is a value.** A column without nulls has the same bytes in both
  layouts, which is what makes the zero-copy paths possible.

- **The ALTREP object is the only R-layout copy of a column.** `Table` once tracked its own
  per-column mirrors next to the ALTREP `data2`, two sources of truth for the same fact. `Table`
  now holds only the Polars frame and its key; residency is read from the ALTREP object.
- **Keyed joins without `on` match x's key positionally against i's key, or against i's leading
  columns when i has no key, and NA keys join each other** (`join_nulls`), all as in data.table.
  `Join.other` is a `Table` rather than a bare `DataFrame` so that i's key reaches the lowering.
  An equi join keeps i's order and then x's (`MaintainOrderJoin::LeftRight`), so several matches
  for one row of i come back in x's order. A rolling join marks x's rows with a
  hidden column so an inner rolling join keeps matches whose values are NA.

- **Native kernels run below Polars, with the Polars plans as the fallback and the reference.**
  `kernel.rs` (row-wise expressions and `:=`), `group.rs` (grouped aggregates) and `join.rs` (equi
  and rolling joins) cover the shapes the db-benchmark exercises and return `None` for anything
  else, and the query then runs as a plan. Polars' lazy engine pays for planning, for widening
  integers to 64 bits to detect overflow, for rebuilding a whole column when `:=` touches a subset,
  and for a per-column `take` that nests parallelism badly, which is where data.table won.
  `query::execute_with_plans_only` skips the kernels, and the differential fuzz in
  `tests/test_table.rs` checks every kernel against it (`FUZZ_ITERS` raises the iteration count).
- **Every `Table` column is one contiguous chunk** (`normalize`, `put_column`, `set_key`,
  `from_normalized`). Kernels read a column as one slice, and the CSV reader returns many chunks,
  so without the invariant every query copied its keys and inputs again (a third of an
  integer-keyed group-by's time).
- **Grouping maps keys to dense codes**: factors by their codes, integers by `value - min`, logicals
  by two values, combined as mixed-radix digits of one `u64`. A code range no wider than the rows
  (and at most 2^20) indexes states directly, and a wider one hash-partitions, because dense states
  cost memory per possible code; dense runs split across threads only when the rows outnumber
  every thread's states. The equi-join kernel bounds its buckets the same way, by the rows on
  both sides. Each aggregate is a `State` that is updated per row, merged across
  threads, appended across partitions and finished per group in output order.
- **Moments use Welford updates with a symmetric co-moment and merge with Chan's formula**, so
  `cor(a, b)` equals `cor(b, a)` bit for bit. They differ from Polars' two-pass result in the last
  few digits, which is why the fuzz compares doubles to 1e-6.
- **Median collects `(slot, value)` entries, counting-sorts them by slot in parallel, and selects
  each group's middle in place**; a valid `NaN` without `na.rm` falls back to the plan, whose
  ordering of NaN matches R's.
- **Join output gathers column by column through `gather_column`**, parallel over rows, with
  factors gathered by code and unmatched rows marked in validity. The rolling join searches
  integer and double times natively after a parallel sortedness check.
- **`mult = "first" | "last"`** keeps one match per row of `i`, as data.table does; `"all"` is the
  default.
- **R's integer `sum` is a double exactly when a total overflows**, as in R 4.3, and a group that
  holds an `NA` (without `na.rm`) is `NA`, not an overflow. A top-level sum keeps 64 bits until
  its totals are known (`narrow_sums`). A sum nested in a selected expression is staged: the
  aggregates first, then the expression (`staged`); after a join, the joined rows are built first
  and the query reruns over them. A new `:=` column nesting one is computed both
  as written and with each sum over doubles (keeping the expression's own type, so `sum(v) > 0L`
  stays logical), and hidden probe sums pick the version
  (`assign` in `query.rs`). Where data.table's grouped `:=` takes the column type from the first
  group, the engine makes the column a double if any group overflows.
- **Non-equi joins and `.EACHI` follow data.table, with one naming exception.** A join condition is
  an `On { x, op, i }`; an ordering compares numbers only (data.table refuses characters too), and
  a rolling join must be all `==`. `join_layout` is the single description of the output columns,
  read by the plan, the kernel and `join_schema`: `x`'s columns with a join column taking `i`'s
  values from its first condition, then one column per further condition on the same `x` column
  (`t.1`, `t.2`), then `i`'s unused columns. data.table names those extra columns `t, t`, a
  duplicate that ry does not reproduce. A name that still repeats (a real column `t.1`, or `i`
  holding both `v` and `i.v`) takes the next free numeric suffix, as `make.unique` would. NA and NaN match only the same missing value, and only
  under `<=`, `>=` or `==`, as data.table does. The kernel (`non_equi` in `join.rs`) groups the
  equality conditions, sorts each group's rows of `x` by the first ordering column and narrows
  every row of `i` by binary search, checking any other condition per candidate. `.EACHI` reruns the
  query as a join with hidden row numbers and groups by the row of `i`, so `j` sees an unmatched row
  as one NA row with `.N` zero, and a column of `i` outside an aggregate is that row's value.
- **Warnings are data in `Executed`, not side effects.** `execute` returns the output with a list
  of `Warning`s, which the interpreter raises as R warnings after the query succeeds, so a failed
  query raises none and the kernels and plans can be compared on them. Two exist, raised where
  data.table raises its own: an integer `sum` that a grouped query (`by` or `keyby`) selects plainly
  or assigns with `:=` and that overflows (one warning per column, none for an ungrouped, nested or
  `.EACHI` sum), and a `:=` into an existing column whose type loses part of the value (a double
  truncated or out of range in an integer column, a number other than 0 or 1 in a logical one).
  NaN becomes NA silently, as in R. In a grouped join, `j` is typed against the joined rows, whose
  join columns carry `i`'s types.
  The wording is ry's. A narrowing `:=` skips the kernel, because detecting the loss needs the
  value before the conversion; it is not a db-benchmark shape. A third warning, `Parsed`, marks
  strings written into a logical or numeric column: data.table warns on every such write, whatever
  the strings hold, so the warning is decided from the types alone, and the strings are parsed as
  `as.logical`, `as.integer` (through a double, so `"3.7"` is 3) or `as.double` would.
- **Prefixed join columns are copies, not a second layout.** `x.t` and `i.lo` in `j` or `by` are
  resolved before the join runs (`prefixed_copies` in `query.rs`): each side gains a copy of the
  named column under the prefixed name, so the copy survives the join as an ordinary column and
  every later stage (kernels, plans, `.EACHI`) needs no knowledge of prefixes. `join_layout` stays
  the only description of the visible output; `result_type` drops the copies from `j = .SD`-like
  results.
- **Under `.EACHI`, an aggregate over a column of `i` alone sees the row's one value.** data.table
  evaluates `j` with `i`'s columns of length one, so `sum(k)` is `k` and `mean(k)` is `k` as a
  double even for an unmatched row. The rewrite (`single_value`) replaces such an aggregate with
  the group's first value, using a hidden `FIRST_OF_ROW` mask where `na.rm` must see exactly one
  value; an aggregate that also reads a column of `x` is left alone.
- **The checker mirrors `join_layout`; it does not call it.** `semantics` must not depend on Polars,
  so `join_columns` in `check.rs` reimplements the layout (names, order, `t.1` suffixes, `i.`
  prefixes, `make.unique`), with the engine as the authority and the typing fixtures
  (`frame_joins`) pinning both against data.table. Condition compatibility follows the engine's
  `comparison_types`: numbers with numbers, strings with strings, logicals with logicals, and an
  ordering never on strings. A factor column is `Any` to the checker, so it is not checked.
- **Under `.EACHI` the checker types `i`'s columns as scalars inside `j`.** Each row of `i` runs
  `j` once, so a column carrying `i`'s values (a compared column or an unused column of `i`) is its
  element type in the scope, while `x`'s columns stay vectors. The groups are the compared columns
  in condition order, matching `each_i` in the engine.
- **A lossy `:=` is a type error, though data.table only warns.** Strings into a logical or numeric
  column, or a fractional literal into an integer column, when the column keeps its type (partial,
  grouped or literal write). The runtime result is almost always a bug (NA or a truncated value),
  and an explicit `as.*()` call states the intent. A number into a string column converts without
  loss and stays accepted, as R code relies on it.
- **`lapply(.SD, f)` is typed once per column, not through `lapply`'s signature.** Typed through
  `lapply`, `.SD` is a list of the union of its column types, so `f` is checked against that union
  (a false error for any `f` that fits each column) and the result has one element type. Instead
  `subset_apply` in `check.rs` types the call once per column with `.SD` read as a one-column list;
  an overloaded `f` passed by name is called directly on each column through `overloaded_call`,
  because an overload set has no single function type for `lapply` to use. A finding every column
  causes is reported once; one only some columns cause names the column.
- **A query result that repeats a column name has unknown columns.** data.table keeps both columns
  (`.SDcols` naming a group column, or `.(g = x)` under `by = g`), but a `frame{...}` type cannot
  hold two fields of one name, and annotations must round-trip.

## Frames as runtime values

**Status:** built, except the class name `"frame"` and `ry` as the standard library's home, which "The DataFrame API is Polars', and ry's own types are UpperCamelCase" and "The standard library is the `std` namespace, with an optional prelude" supersede.

- **A frame is an `EXTPTRSXP` that owns a `Box<table::Table>`, tagged with the symbol `ry.frame`
  and classed `"frame"` alone** (`crates/frames/src/frame.rs`). A weak reference with a C
  finalizer drops the box. The table is the only copy of the data: a column becomes an R vector
  when R code reads it (`$`, `[[`, `as.list`, a data mask). It is no longer a `data.frame` (the
  class was `c("frame", "data.frame")`), because nothing about a data.frame's list layout or row
  names holds for it, and the checker agrees: a frame fails a `data.frame` parameter but passes a
  `list` one, because `as.list` gives its columns.
- **`x[i, j, by]` on a frame never reaches the `[` primitive.** `[` is a `DispatchOrEval`
  special, so `dispatch::dispatch_or_eval` evaluates only the subject, and when it is a frame
  hands the unevaluated call to `frame::run`. Dispatching through `[.frame` would be too late,
  because `i`, `j` and `by` name columns. `[.frame` still exists for calls that arrive with
  evaluated arguments (`do.call`, `Map`). The bytecode compiles `[` as a generic call, so loops
  that subset run the same path.
- **Any other object subject of `[` dispatches lazily, as GNU R's `DispatchOrEval` does.** The
  method receives the remaining arguments as promises, so `DT[, sum(x), by = g]` reaches
  `[.data.table` with expressions instead of failing with "object 'x' not found".
- **libr's option reads (`Rf_GetOption1`, `Rf_GetOptionWidth`, `Rf_GetOptionDigits`) go to the
  interpreter's `options()` when the interpreter is installed.** The `.Options` pairlist in base
  is the one source of truth; libr's own options environment serves only standalone use without
  the interpreter. With two stores, C code sees no option R code set, and data.table's `alloccol`
  fails with "Has getOption('datatable.alloccol') somehow become unset?".
- **Lowering first, R second.** `Lowering` turns `i`, `j` and `by` into the engine's IR; any part
  that returns `None` runs as R in a data mask that binds only the columns the expression
  mentions, plus `.N`, `.SD`, `.I`, `.GRP`, `..x` and `.` as `list`. Grouped R code uses Polars
  for the grouping (row indices per group) and evaluates once per group.
- **Copy-on-modify, not by-reference.** `:=` updates in place when the variable the call names
  holds the frame's only reference, and otherwise forks the table (cheap: Arrow buffers are
  shared) and rebinds that variable, searching enclosing scopes, so a global updated inside a
  function is rebound where it lives. The only reference reads as a count of two, because the
  finalizer every frame carries counts as one (`assign` in `crates/frames/src/frame.rs`). An
  alias taken before the update keeps the old columns, as the checker models.
  `DataFrame.set_key` always forks and rebinds.
- **`ry` is a registered namespace attached as `package:ry` at position 2** (`frame.rs`,
  `NAMESPACE`, `PACKAGE`, `ATTACH`), built in the shape `loadNamespace` gives an installed package,
  so `ry::DataFrame` and `ry::List.map` work after another package masks the bare name. It is the one
  home of the standard library and the frame functions: the builtins bind the standard library in
  a `package:ry` just above base, and session startup moves those bindings into the namespace and
  replaces that environment with the full package in front of the default packages. It holds the
  user-facing functions; `.frame_*` builtins do the work. S3 methods for other packages' generics
  are registered in the generic's methods table, as an installed package's `S3method()`
  directives are; `as.data.table.frame` is registered from a data.table `onLoad` hook, because
  data.table is not loaded at startup. The checker's stubs are `types/ry.Rtypes`
  and `types/ry.exports`, and
  `test_standard_library.rs` keeps both equal to what the runtime binds.

## data.table runs on its own code; the `[` bridge is removed

**Status:** built, except `ry::setkey`, `setkeyv`, `key` and `copy`, which "package:ry attaches no bare name another package exports" supersedes.

- **Previous source of truth.** `crates/interp/src/data_table.rs` answered `[` on a plain data.table on ry's table engine and declined to data.table's own `[.data.table` whenever it doubted. Two implementations of data.table's semantics (values, attribute order, warnings, errors, by-reference state) lived side by side, and nothing but hand-written oracle cases compared them.
- **What was structurally weak.** Declining only works if the bridge predicts every place data.table behaves its own way, and that set is open-ended: warnings and errors are worded by data.table, state such as over-allocation and indices belongs to its C code, and each query shape is another rule to copy. It was also a data.table special case inside the runtime, which the user directive rules out (data.table is a normal package).
- **Measured result.** A seeded differential (R script run in one ry process, each query with the bridge on and with `options(ry.datatable = FALSE)`, compared by `identical()` on value, attributes and their order, both tables afterwards, printed output and every condition) ran 7,500 queries over five seeds. The bridge answered 1,028 of them, and 74 of those (7.2%) differed from data.table. The classes: a join where data.table stops with its cartesian-product error and the bridge returned rows (`o[dt, on = "g"]`); data.table's warnings lost (`.SDcols` given with a `j` that never uses `.SD`, `max()` of an empty group, `>` on a factor); a `max` over a character column that data.table rejects; and the bit pattern of `NA` in a double column built from an integer `NA` (`h + 1`). Roughly five distinct causes in the first few thousand queries, from a generator that covers only part of the query surface, so more would follow.
- **Target shape.** The bridge, its dispatch hook, its code-image state, the strict-lowering mode of the frame lowering that only it used, and the masking hook that copied `setkey`, `setkeyv`, `key` and `copy` into `package:data.table` are gone. `ry::setkey`, `setkeyv`, `key` and `copy` now accept only a DataFrame; the data.table functions are called as `data.table::setkey`. The `as.data.table` method for DataFrame stays, because it converts the DataFrame, not a data.table query. A `[` on a data.table always runs data.table's code.
- **Impact.** Correctness: a data.table result is data.table's by construction. Speed: grouped queries on a data.table lose the engine; the DataFrame is the fast table, chosen for new code. The `datatable__*` oracle cases stay as checks of data.table's own code under ry. Analysis: none. The differential harness was removed with the bridge because both of its runs now take the same path.

## Delimited files: one sniffer for the reader and the checker

**Status:** built, except "fread returns a frame", which "data.table's helpers run data.table's own C, and the C API is what makes them fast" supersedes.

- **Previous shape.** `fread` was declared to return the `data.table` nominal, so a read file had no column knowledge, and the table engine had no reader or writer of its own.
- **Target shape.** `crates/sniff` decides a delimited file's layout (separator, header, column names, and each column's type out of logical, integer, double and character) with fread's rules, from the head plus jump points spread over the file. It is the single source of truth for that layout: `table::io::read_csv` parses with it, and the hosts sniff every project `.csv`/`.tsv` head (64 KiB, `sniff_head`) into the `DataFiles` salsa input, which `fread("path")` reads through the per-path projection `data_file_layout`. A file whose sample missed a wider value rereads that column from all its values, so the reader never gives a narrower type than the data holds.
- **fread returns a frame.** ry's table replaces data.table, so `fread`, `rbindlist`, `melt` and `dcast` return `frame`, and a frame satisfies a `data.table` parameter. `data.table()`, `as.data.table` and `setDT` keep the nominal. The checker refines these calls after the ordinary call has checked their arguments (`refine_table_call` in `check.rs`), reading only the recorded argument types, so nothing infers twice. `setnames` and `setcolorder` reuse the `:=` slot-write model (`naming::record_column_update`).
- **Writer.** `fwrite` output matches data.table's text: shortest round-trip doubles (ryu, `1` not `1.0`), `TRUE`/`FALSE`, `NA` as an empty field, and quoting only where needed. Batches format in parallel and write in order.
- **Known gaps.** integer64 reads as double, dates read as character, `dec = ","` and `strip.white` on character columns are not supported, and a ragged line is an error rather than fread's fill. The checker resolves a data path against the project root only, because `SourceFile` carries no path.
- **Impact.** Editing a data file invalidates only the checks that read its path (salsa backdates every other path's layout). The CLI walks data files without `.gitignore`, since data directories are commonly ignored.

## The table engine's sort and distinct kernel

**Status:** built.

- **Previous shape.** `setkey`, `setorder`, `unique`, `uniqueN` and `dcast` called Polars' multi-column sort and `unique_stable`, which compare row by row through the column types, cannot match data.table's NA, NaN and `-0` rules without extra passes, and lost to data.table at 1e7 rows.
- **Target shape.** `ops::order_digits` codes each key column as one `u64` per row whose unsigned order is the sort's order: `NA` is 0 and `NaN` is 1, both first in either direction as data.table places them; integers are offset from their minimum; doubles go through `ordered_bits`, so `-0` ties with `0`; factors use their codes; strings use their rank in byte order (C locale) from `string_ranks`. `sorted_rows` packs a row's digits and its row number into one `u64` or `u128` (`Packed`), and `radix_sort` sorts only the digit bits: up to three stable LSD counting passes, or for wider keys one MSD pass on the top 16 bits followed by an in-cache sort of each bucket. The row number makes every key distinct, so the result is stable. Keys wider than 96 bits fall back to Polars.
- **Distinct rows.** Keys of 24 bits or fewer skip sorting: `uniqueN` sets bits in a shared atomic bitmap and `unique` keeps a first-row table per task. Wider keys sort and compare neighbours.
- **String ranks.** Each thread numbers the strings of its slice in a hash map, keyed by the 16-byte view itself for strings of 12 bytes or fewer (no string read), with a small direct-mapped cache in front while the slice has few distinct strings. The distinct strings of all threads sort once by `(first 16 bytes as a big-endian integer, string)`, which is byte order with almost only integer comparisons.
- **Gathers.** Reordering a frame gathers every column in parallel; a string column gathers its views and shares the original data buffers, so no string bytes move.
- **Impact.** One kernel serves every ordering operation, so all of them agree on NA placement and ties, and `crates/table/tests/test_table.rs` (`fuzz_sort_unique_and_runs_against_a_reference`) checks them against a reference comparator over random NA, NaN, `-0`, infinities, factors and long shared-prefix strings.

## The DataFrame API is Polars', and ry's own types are UpperCamelCase

**Status:** built, except routing plain data.table queries to the engine, which "data.table runs on its own code; the `[` bridge is removed" supersedes.

- **Previous shape:** the built-in table was `frame`, queried only through data.table's
  `x[i, j, by]`, and ry's own types were lowercase (`frame{...}`, `record{...}`, `tuple{...}`), so
  nothing in a type told R's types from ry's.
- **Names:** every type and type constructor ry adds to R is UpperCamelCase (`DataFrame`,
  `LazyFrame`, `GroupBy`, `LazyGroupBy`, `Expr`, `Record{...}`, `Tuple{...}`, alongside the
  standard library's `Result`, `List`, `Str`, `Dict`, `Set`), and R's own types stay lowercase.
  The case is the visible line between R's types and ry's. No lowercase alias was kept: the old
  names had no users outside this repository, so a deprecation path would only add a second
  spelling to every renderer and fixture.
- **API:** Polars' verbs over expressions, as qualified functions used with `|>`:
  `DataFrame.select`, `with_columns`, `filter`, `group_by` + `GroupBy.agg`, `sort`, `join`,
  `head`/`tail`, `lazy`, and the `LazyFrame.*` mirror with `collect` and `explain`. Expressions are
  `Expr.col("x")`, `Expr.lit(v)`, R's operators through `Ops.Expr`, and `Expr.*` functions (`alias`, the
  aggregations, `len`, `is_null`, `if_else`, `cor`). There is no method dispatch: a qualified name
  always means one function, which keeps the checker's and the IDE's view a plain name lookup. A
  string names a column wherever a verb takes an expression, as in Polars.
- **One plan for eager and lazy:** every verb appends a `table::lazy::Step` to a `Plan`, and an
  eager verb collects at once. The optimizer pushes filters below `with_columns` and projections
  and into a scan, prunes scan columns, and turns a leading `head` into a scan row limit;
  `crates/table/tests/test_lazy.rs` fuzzes that optimizing never changes a result. Errors name the
  schema of the longest valid prefix (`Plan::valid_schema`), so a misspelled column lists the
  columns that exist at that step.
- **Typing:** the checker refines a verb call after the ordinary call has checked the arguments
  against `types/ry.Rtypes` (`refine_dataframe_call` in `crates/semantics/src/check.rs`), reading
  each argument's syntax for its output name and column type. The refinement applies only when the
  name reaches the library (`reaches_library`), so a project's own `DataFrame.filter` is an
  ordinary function. An argument the checker cannot read (an `Expr` in a variable) leaves the
  columns unknown rather than guessing. `as.data.table` and `as.DataFrame` keep the columns.
- **data.table syntax stays as compatibility, typing included.** The runtime still answers
  `x[i, j, by]` on a DataFrame and routes plain data.table queries to the same engine, so existing
  code keeps working; the bracket typing already existed and costs nothing to keep. The docs teach
  the verbs and present the brackets only as compatibility.
- **R-side classes:** the value's class is `"DataFrame"` alone; `as.data.frame` and
  `as.data.table` methods are registered with `registerS3method` into the generics' namespaces, so
  they work whether or not the package is attached first.
- **Impact:** column checks and completion cover the whole query rather than one bracket;
  incremental analysis is unchanged, since the refinement reads only the item's own recorded types.

## Per-group top rows and as-of joins reuse the engine's kernels

**Status:** built.

- **Previous shape.** The Polars-style API had no way to keep each group's best rows or to join on
  the nearest key, so the table bench answered db-benchmark's top-two question and the keyed
  rolling join with data.table syntax (`x[order(-v3), head(v3, 2L), by = id6]`,
  `prices[trades, roll = TRUE]`).
- **`Expr.head(n)` inside `agg` keeps up to `n` values per group, as rows.** Polars gives a list
  per group there, but ry has no list columns, so the values are exploded into rows, which is also
  data.table's layout. The checker tracks an expression's shape (`Shape` in
  `crates/semantics/src/check.rs`: per row, per group, or up to `n` per group) and accepts `Head`
  only as a whole `agg` item, because `head(v, 2) + 1` would mix shapes. After a sort, the planner
  fuses sort and aggregation into one query, which hits the engine's top-k kernel
  (`group.rs::top_k_shape`) instead of sorting the whole table.
- **`join_asof` is `How::Asof(Strategy)` in `table::lazy`, lowered to the rolling join.** The left
  frame is data.table's `i`, so `"backward"` is `roll = TRUE`, `"forward"` is `roll = -Inf` and
  `"nearest"` is `roll = "nearest"`. The `by` columns become the leading equality keys and `on` the
  rolled key. One kernel (`join.rs::rolling`, with Polars' asof as the fallback) serves both APIs.
- **A grouped median holds `(row, slot)` pairs, one list per run of rows.** It used to hold
  `(slot, value)` pairs and concatenate the threads' lists, 240 MB of fresh memory per 1e7-row
  query; page-faulting that in made groupby q6 from R two to ten times slower than the same kernel
  measured from Rust. Reading the value from the input column during the counting sort halves the
  lists and drops the concatenation.
- **Impact.** No new kernel or stored state; both verbs are thin plan steps. The typing fixtures in
  `frames.R.test` and `crates/table/tests/test_lazy.rs` pin the semantics.

## data.table's helpers run data.table's own C, and the C API is what makes them fast

**Status:** built, except the `[` bridge, which "data.table runs on its own code; the `[` bridge is removed" supersedes.

- **Previous shape.** The plan was to route R calls to `fread`, `fwrite`, `setkey`, `rbindlist`, `melt`, `dcast`, `shift`, `fifelse`, `tstrsplit` and friends to the engine's kernels in `table::ops` and `table::io`, which beat data.table when measured from Rust. Under ry's runtime those calls ran data.table's own C through libr, at up to 2.5 times GNU R's time, because every C API call (`TRUELENGTH`, `STRING_ELT`, `LENGTH`, `INTEGER`, the type predicates, `PROTECT`) entered the full error boundary.
- **What measurement showed.** With the boundary skipped on those calls, data.table's C under ry runs at GNU R's speed or faster: `fread` of the 1e7-row groupby file takes 2.2 to 2.3 s against GNU R's 2.8 to 3.8 s (best of two, across separate runs), and `transpose` (the core of `tstrsplit`) 0.062 s against 0.055 to 0.068 s at 1e6 rows. Routing `fread` to the engine's reader made it slower, 3.2 to 3.7 s: the reader itself takes 1.4 s, but turning its columns into R vectors (one CHARSXP per string) costs about 1.9 s. The numeric helpers are already a `memmove` or a single loop in data.table's C, so the engine has nothing to win there.
- **Target shape.** No routing hook for the helpers. They stay data.table's own code, so their output is data.table's by construction. Their speed comes from the C API (`crates/libr/src/vectors.rs` and `memory.rs`): an accessor whose argument is a plain, non-ALTREP object of the right type reads it directly, and enters the boundary only to raise an error. Every package with compiled code gains from this, not only data.table. The engine is fastest where data stays inside it: frames (`frame_read_csv` reads the same file in 1.4 s) and the `[` bridge, whose results are small.
- **Impact.** One source of truth for each helper's semantics, data.table's own code. The checker still types `fread`, `rbindlist`, `melt` and `dcast` as returning frames, which is not true at run time (`backlog.md`, "The checker and the runtime disagree about data.table's readers").

## Frames and the checker share one CSV reader, and data.table keeps its own

**Status:** built, except reading factors back with `convert::factor_levels`, which "Factors are ry's own codes plus one level table, outside Polars" supersedes.

- **Rejected alternative.** A faster frame read path could have been a second, lighter CSV reader beside the engine's, with its own type inference and quoting rules, so that the checker's schema and the runtime's columns could drift apart.
- **Target shape.** The engine has one CSV reader: `table::io::read_csv` / `read_csv_with`, through `read_bytes` and `read_sliced` (`crates/table/src/io.rs`). The `sniff` crate decides the layout and the column types for both that reader and the checker (`semantics/src/check.rs`). A lighter or faster path is a change inside this reader, not a new one, and character columns read as factors go through the reader's own factor path (`io::strings_as_factors`, read back with `convert::factor_levels`).
- **data.table's helpers stay on data.table's own C.** No helper is routed to the engine, `fread` included (see "data.table's helpers run data.table's own C"). Measured at 1e6 and 1e7 rows, `fread`, `fwrite`, `rbindlist`, `setkey`, `setorder`, `unique`, `uniqueN`, `melt` and `dcast` all run at or below GNU R's time under ry. Each figure is the best of five runs of the call `tools/bench/internal/table/ops.R` makes; `ops.R` itself keeps the faster of two runs, which can show `setorder`, `unique` or `uniqueN` up to 1.25 times slower on either engine, and repeated runs do not confirm that. A helper moves to the engine only if a measurement shows the engine winning end to end, including the conversion back to R vectors.
- **Impact.** One source of truth for CSV parsing and type inference on the engine side, and one for each data.table helper's semantics: data.table's own code.

## A DataFrame column reads back as a plain vector, not an Array

**Status:** built.

- `DataFrame(price = [12.5, 9.99])` checks each column as an Array, then stores it as the plain
  vector, and `df$price` reads back a plain `double[]`, statically and at run time.
- Typing the read as `Array<T>` was rejected: a frame's later steps can put `NA` into a column (a
  left join's unmatched rows), so the column type would have to be tracked per verb and an
  `NA`-holding column would need a second representation. One column type keeps the schema the
  single source of truth. `as.Array(df$x)` is the checked way back, and it fails on an `NA`.
- A narrower rule was also rejected: reading an Array only when the checker can prove no step put
  an `NA` in (no left or outer join, no `NA`-producing expression). An Array changes how R
  arithmetic behaves (no recycling, no `integer`/`double` mixing), so the same `df$units *
  df$price` would pass or stop with an error depending on which verbs ran before it, and a
  refactor that adds a join would break unrelated arithmetic. The rule is in `type-system.mdx`
  ("DataFrames").

## The engine's workers return freed memory at large frame boundaries

**Status:** built.

- **Before.** Nothing returned memory that the engine's worker threads had allocated. libR's
  allocator is mimalloc, which queues a block freed on another thread for its owning thread and
  processes the queue only when that thread allocates again. Polars' and rayon's workers allocate
  a table's columns, the R thread frees them, and the pools then sit idle, so a 5e7-row
  `DataFrame.read_csv(strings_as_factors = TRUE)` left 9 GB resident for a 1.7 GB table, and the
  db-benchmark group-by script ran out of memory on a 15 GB machine where data.table peaked at
  6.4 GB.
- **Shape.** `table::on_every_worker` runs a closure on every thread of both pools;
  `frames::frame::release_worker_memory` runs `mi_collect(true)` there and on the R thread. It runs
  when a frame of at least 2^22 cells is wrapped or finalized, and in `gc()`. The size floor keeps
  loops over smaller frames from paying for a broadcast per iteration.
- **Trap.** A broadcast waits for every pool thread, and a forked child has none of its parent's
  threads, so a child that drops a large frame or calls `gc()` would hang. Whoever implements
  `fork` (`mcparallel`, `mclapply`) must skip the broadcast in the child.
- **`mi_collect(true)` drains each thread's queue of blocks freed elsewhere,** so their pages
  become reusable, and that reuse is what bounds the peak. It also returns the pages whose purge
  is scheduled ("libR keeps freed memory until `gc()`").
- **Rejected.** `MIMALLOC_PURGE_DELAY=0` only purges pages already freed, so it left 5.9 GB; a
  per-query arena would also fix it but needs every kernel to allocate through it.

## Group codes wider than 64 bits are ranked, not handed to Polars

**Status:** built.

- The group-by kernels code the keys as one mixed-radix u64 per row. db-benchmark's six-key
  question at 5e7 rows needs about 2.6e19 codes, so the kernel used to decline, and Polars' own
  multi-key group-by then took more than 12 GB.
- `row_codes` (`crates/table/src/group.rs`) now replaces the codes so far by their rank among the
  distinct codes before a key column that would overflow. A rank keeps the code order, so `keyby`
  stays sorted, and it is below the row count, so any number of key columns fits as long as each
  fits on top of the row count. The question takes 7 s at a 6.2 GB peak; data.table takes 10.5 s.

## Double sums group by code range, one scan per thread

**Status:** built.

- A double `sum` or `mean` adds each group's values in row order, as data.table does, so the
  dense group-by kernel cannot split the rows into chunks and merge partial sums (the result would
  round differently and depend on the thread count). It used to run such a query on one thread,
  which at 5e7 rows and 500,000 groups missed cache on almost every row and lost db-benchmark's
  q3 and q5 to data.table.
- `dense_by_code` (`crates/table/src/group.rs`) gives each thread a contiguous range of codes; the
  thread scans every row, keeps its own in ascending order, and the threads' states concatenate in
  code order. The same path serves any dense grouping whose per-thread states would outnumber
  the rows.
- Rejected: scattering the rows into many code-range partitions first, as the hashed path does.
  A partition's rows are spread over the whole table either way, so each partition reads every
  cache line of every value column, and the scatter is a pass of its own. Measured from the Rust
  driver at 5e7: q3 1.42 s scattered against 1.18 s scanned, q4 1.71 s against 1.19 s. Keeping
  one thread below a cache-sized slot count was slower too (q4 1.42 s).

## Factors are ry's own codes plus one level table, outside Polars

**Status:** built.

- **Previous shape.** A factor column was a Polars `Enum`; `string_to_factor` built the levels
  (parallel hash partitions, byte-order sort) and then called `FrozenCategories::new`, which
  interned every level a second time at roughly 135 bytes per level. The CSV reader parsed factor
  columns as `Categorical` with a shared mapping and re-read a column whole as text when it held
  an `NA`. At db-benchmark's 1e8 join inputs (up to 1e8 distinct levels) the Enum alone kept
  `join_x` from fitting next to `join_big` in 15 GB.
- **Target shape.** `crates/table/src/factor.rs`: a factor is a Polars extension type
  (`ry.factor`, feature `dtype-extension`) whose storage is a dense zero-based code column
  (u8, u16 or u32 by level count) and whose dtype holds one `Arc<Levels>`, the labels in R's
  order (see "A factor's level table is front-coded in byte order, with no hash index").
  Extension series filter, take, sort, group and append through their storage, and
  `to_physical_repr` gives the codes, so
  `kernel::Codes::of` and the row encoder need no factor case. Casting an extension column is
  refused by Polars, so every conversion goes through `factor::cast`, `factor::text` or their
  lazy forms `cast_expr`/`text_expr` (Polars `map` UDFs).
- **Reader.** A factor column parses as String slice by slice, and each slice is interned into a
  persistent `Interner` with 256 hash partitions, each owning its labels' views and byte arena (a
  label of up to 12 bytes lives in its view). Codes during the read are `index << 8 | partition`.
  A partition that the first slice fills with mostly distinct labels is sized for the whole file
  from that slice, because growing it later copies and rehashes every label and holds both tables
  at once. `finish` sorts one 16-byte key per level (the label's first 12 bytes read big-endian, then
  its code), stored in the view type so that the sorted keys become the label views in place, re-sorts only the runs
  whose prefix ties, remaps the codes in parallel, and turns the arenas into the label buffers
  without copying a byte. `NA` never enters the levels, so the whole-column text re-read is gone.
- **Joins.** `join::factor_codes` translates one side's codes into the other's levels through
  `Levels::translation`; identical level tables (the same `Arc`) translate by identity.
- **Impact.** One source of truth for a column's levels and no second interning pass.

## A factor's level table is front-coded in byte order, with no hash index

**Status:** built.

- **Previous shape.** `Levels` held a `Utf8ViewArray` (a 16-byte view per label, plus a byte
  buffer for labels over 12 bytes) and a hash index from label to code that the first lookup
  built. The views were a second copy of what the sorted order already says: db-benchmark's
  `id6` labels are `id1`..`id100000000`, and neighbors in byte order share all but the last
  byte or two. At 1e8 rows `join_x` has 63.2 million `id6` levels, so the views alone were
  1.0 GB of a 3.9 GB table, and the index, once a join or a recode built it, was derived state
  that could not be shared between tables.
- **Target shape.** `Levels` in `crates/table/src/factor.rs` stores the labels sorted by bytes
  and front-coded in blocks of 16. An entry is a header byte (four bits for the bytes shared
  with the previous label, four for the bytes that follow, 15 meaning a LEB128 length follows),
  then the rest bytes. A block's first entry shares nothing, and `blocks` holds each block's
  byte offset. A reader builds levels in byte order, so a code is a rank. A table whose code
  order is not byte order (`factor(levels = ...)`, a level union) adds `Order`, two `u32`
  arrays between code and rank.
- **Lookups.** `position` binary-searches the block heads and scans one block, so there is no
  hash index to build, hold or keep in sync. `translation` maps one table's codes into another
  with a merge of two cursors, which replaces one hash lookup per level in a factor join and in
  `recode`. `text` decodes a whole table in parallel when the column has at least a sixteenth as
  many rows as levels and seeks per row otherwise, so printing ten rows of a table with 63
  million levels decodes ten blocks.
- **Build.** `Interner::finish` sorts one 16-byte key per level, then encodes runs of
  blocks in parallel and joins the pieces. Runs of tied 12-byte prefixes sort in parallel.
- **Impact.** In `tests/test_memory.rs`, `join_x` holds 32 bytes per row where it held 39; the
  budgets there pin it. One source of truth for label to code, and less memory for every
  factor with sorted, similar labels. Labels that share nothing cost their bytes plus a byte
  and a half each (the header, and a sixteenth of a block offset).

## Equality joins bucket one side by dense codes of its own key range

**Status:** built.

- **Previous shape.** `join.rs` coded both sides' keys over the union of their value ranges,
  chose the bucketed side afterwards, and refused any code space above 2^27 to the Polars hash
  join. db-benchmark's q5 at 1e8 rows joins on `id3` over a range of 2e8, so it fell back, and
  the hash join's tables did not fit beside the inputs in 15 GB. The bucket index was one
  `Vec<u32>` per side with a separate start array, built on one thread.
- **Target shape.** The bucketed side is chosen from the row counts before any key is coded
  (`i` when it has at most 65,536 rows and no more than `x`, otherwise `x`, which is the right
  table of `DataFrame.join`), and keys are coded against that side's range alone, so a key of the other side outside the range gets a code no built row
  has. `indexable` admits a range up to eight times the rows below 2^27 codes and up to twice
  the rows above. `Buckets` is two flat arrays (`bounds`, `rows`), filled by tasks that each own
  a share of the code range. `scan_i` counts matches per chunk, prefix-sums and writes the row
  pairs in parallel, and `assemble` gathers one side's columns, frees that side's row indices,
  then gathers the other.
- **Impact.** Every db-benchmark join at 1e8 rows runs in the kernel. The transient memory of a
  join is the bucket index and one side's row indices; `tests/test_memory.rs` pins it.

## A filtered `:=` reads a row index and writes in place

- **Previous shape.** `x[column == value, target := ...]` compared the whole filter column
  against the literal on every call, and from R it also copied the whole target column on every
  call. The copy was a reference-count mistake: the finalizer that `wrap` registers for a frame
  holds one reference, so a frame that one variable refers to counts two, and `assign` in
  `crates/frames/src/frame.rs` forked the table unless the count was one. The fork's column was
  shared with the frame the variable still held, so `scatter` copied it, and the replaced frames
  stayed allocated until the next collection.
- **What was weak.** A loop of small updates cost the table's length per pass twice over, which
  made db-benchmark's "`:=` on 1% of rows, 100 times" the one question data.table won. data.table
  answers it with a secondary index it builds on the first `id4 == step`.
- **Target shape.** `assign` writes in place when three things hold: the variable's own binding
  cell holds the frame (so an active binding, whose function may return a frame out of a shared
  list, never qualifies), the count is at most two, and no outer `x[i, j, by]` query of the same
  frame is still running (`frame_queries` in the interpreter state lists the running subjects,
  because an update nested in an argument of such a query would change rows the outer query is
  reading). Otherwise the update goes to a fork that is assigned back to the variable, through
  the active binding's function when there is one. Either way the update is stored before its
  warnings are raised, so a handler that unwinds on the warning sees the same frame whether or
  not it was shared. An update that fails changes nothing, and with no fork to drop that is each
  path's own job: the kernels and the plan compute every column and check every removal before
  they store one, and the path that evaluates the values in R (`fallback` in `frame.rs`) stores
  its columns into a fork that replaces the table only after the last one, because a removal of
  a missing column fails there after earlier columns are stored. The fork shares the columns it
  keeps, so it costs a column list. `Table` keeps
  `equality_filters`, a short list of columns with the state of each: scanned once, indexed, or
  not indexable. `Table::row_index` builds a column's `RowIndex` on the second equality filter
  since the column last changed, so a one-off update never pays for an index. A `RowIndex` is the
  join's `Buckets` (`kernel.rs`) over the column's values, built with one stable parallel
  counting sort, which keeps each value's rows ascending. An Integer column qualifies when its
  value range is at most twice the rows plus 4,096, and a Factor column when its level count is
  (a ten-row slice of a frame with three million levels would otherwise pay 12 MB of bucket
  bounds). `assign_with_kernels` in `query.rs` checks every assignment's type before it selects
  rows and every value's shape (`kernel::scatters`) before it writes, so an update the kernels
  cannot run builds no index and leaves no half-written column. `indexed_rows` recognizes
  `column == literal` in either order and reads the rows; every other filter scans as before.
  `scatter_values` writes a long ascending row list on the worker pool by carving the column into
  disjoint stretches, and takes the serial, bounds-checked path when the last row lies outside
  the column.
- **A builtin S3 method releases the promises its dispatch made.** `dim(x)`, `length(x)`,
  `names(x)`, `x$v` and `x[["v"]]` on a frame dispatch to builtins, and the dispatch wraps each
  argument in a forced promise that counts a reference to its value. No frame owns those
  promises, and a collection that frees one does not lower its value's count, so every such
  call left the frame looking shared and the next `:=` copied it. `apply_method` in
  `dispatch.rs` clears them when the builtin returns (`Heap::release_argument`).
- **A detached argument reference gives up the pool's count, and the pool hands out no
  reference that something else still counts.** A compiled closure takes a variable argument as
  a pooled reference, and a call that needs the frame's environment (`dim(x)` inside `nrow`, any
  S3 dispatch) turns the reference into an ordinary promise in place (`detach_reference` in
  `eval.rs`). A returning frame clears a promise only when the frame alone counts it. The
  promise kept the count the pool held on it, so it stayed forced and the argument's count rose
  by one per call: twenty `nrow(p)` calls on a classed list read a count of 20, and a loop that
  called `nrow(x)` between updates copied the column in every pass (100 passes over 4e6 rows:
  1.04 s, against 0.077 s with the count dropped at the detach and 0.071 s without the call).
  `pooled_reference` also replaces a reference whose count is above the pool's own, which is one
  that the `...` list of an unreleased frame still counts (next entry). The replacement costs
  one promise and one three-integer target, and it is rare: only a frame whose call failed, or
  one whose release waits for a reconcile, leaves such a count.
- **A released frame empties its `...` list.** Previous shape: `clear_frame` and
  `release_slots` (`heap.rs`) released a frame's promises but only read its `...` list to
  release the promises inside, so the list kept its count on every argument. Weakness: the list
  is garbage once its frame is released, and the collection that frees it lowers no count, so
  the counts never came back. A pooled argument reference that `cat(a)`, `paste(a)` or
  `list(...)` had collected stayed counted, and the next call that got the same reference from
  the pool and detached it kept a promise no frame could release, with the argument's value
  counted for good. Replacing every such reference in the pool hid that, at the price of a
  promise and a target allocated on every call that followed a `...` call. Target shape:
  `Heap::release_dots` walks the list of a frame being released, drops each cell's count on its
  argument and empties the cell, and stops at a cell something else counts. Impact: a `...`
  call returns the counts it took, so the pool reuses its references (a loop of 1e6 passes that
  calls a `...` function and then a two-argument closure runs 26 collections, the same as
  before the pool replaced anything), and a `:=` subject passed through `...` stays the sole
  reference.
- **Printing a value at top level rebinds it to `NULL` afterwards.** `print_value_top` and
  `print_dispatch` (`print.rs`) bind the value as `x` in an environment made for the one
  `print(x)` call. The collection that frees that environment lowers no count, so each printed
  frame kept one count and the next `:=` copied it. `release_printed` rebinds `x` to `NULL` when
  the call returns, as GNU R's `PrintObjectS3` does.
- **A value on its way to the caller is counted while exit code runs.** Nothing counted a
  function's result between the body and the caller, so `on.exit(x[1] <<- 0L); x` returned the
  modified vector where GNU R returns the one the body produced, and under the in-place rule
  exit code that updated a frame the function was returning changed the returned frame.
  `run_on_exit` (`eval.rs`) raises the result's count before the exit expressions and lowers it
  after them, as GNU R's `endcontext` does, so such exit code writes to a copy. Pinned by the
  oracle case `environments__on_exit_keeps_returned_value` and the scenario.
- **A locked binding takes the fork.** The in-place write stores nothing in the binding, so it
  would have changed a frame behind `lockBinding` or a locked namespace. `assign` treats a
  locked cell like an active one, and storing the fork raises R's "cannot change value of
  locked binding" before the variable changes.
- **A `...` list that outlives its frame keeps its arguments.** `get("...")` and
  `environment()$...` return the list itself, uncounted. `clear_frame` asks its caller which
  lists to keep: the call's result when the frame is released at return, a list a root holds
  when a reconcile releases it, and a marked list when the sweep does. Pinned by the oracle
  case `environments__dots_list_returned_from_its_frame`.
- **An update nested in an update of the same frame is replaced by the outer one.**
  `x[id == 1L, v := { x[id == 2L, v := 0L]; 5L }]` sends the nested update to a fork, because
  the outer query is running. The outer update then finds another frame in its variable, forks
  the frame it began with and stores that, which is what `x <- update(x)` does when computing
  the update reassigns `x`. The scenario pins it.
- **The index is a secondary index, not a cache of a slow path.** The first filter still scans,
  and the scan is as fast as it was. The index answers a different question (all rows of a value)
  that a repeated filter asks, the way `string_codes` answers a repeated join on a text column.
  It has one owner and one invalidation point: `forget_column`, which every path that takes,
  puts, replaces or drops a column already calls, plus `set_key`, which reorders the rows. Forks
  share it through an `Arc`.
- **Cost.** 4 bytes per row for the row list plus 4 bytes per distinct value, held until the
  column changes or the table is dropped. `test_memory.rs` pins the loop at 5 bytes per row, both
  at its peak and held afterwards (measured 4.1 and 4.0).
- **Impact.** db-benchmark's "`:=` on 1% of rows, 100 times" on 4 cores, the faster of two runs:
  at 1e7 rows 0.111 s from R and 0.105 s from Rust against data.table's 0.356 s (2.414 s and
  0.628 s before), and at 1e8 rows 0.987 s and 1.040 s against 3.079 s. The run that builds the
  index took 0.26 s at 1e7 rows and 3.0 s at 1e8, against 0.40 s and 4.1 s for data.table's
  first run. The whole 1e7 suite's peak from R fell from 8.4 GB to 4.5 GB; which loop held the
  difference was not isolated.
- **Cost of the exact counts in the interpreter.** The testthat workload under callgrind runs
  0.6% more instructions per warm repeat (548.3M to 551.7M, `RY_BENCH_REPEAT` 3 minus 1,
  halved), with the same number of collections and the same resident memory. A per-function
  comparison of the two builds puts it in the releases that were skipped before. Over a run of
  three repeats, about 31,000 `...` lists are emptied (`release_dots`), about 120,000 more
  values reach a count of zero when their frame returns instead of waiting for a collection
  (`note_zero`), and `release_slots` makes one more comparison per slot, because it looks for a
  `...` list beside a promise.
- **Known limit of the count.** A holder the counts do not see, such as native code that keeps
  the frame through `R_PreserveObject` or the protect stack, does not stop an in-place write.
  No DataFrame builtin keeps a frame that way.
- **Not done: a row index for a filter that runs once.** A single `x[id == value, ...]` scans.
  Building the index reads the column twice and writes a row list as long as it, which is more
  work than the one scan it would replace.

## A query without `by` aggregates in the kernel as one group

**Status:** built.

- **Previous shape.** `DataFrame.select(sum(...))` and `x[, sum(v)]` went to Polars, where a
  double sum materialized a cumulative sum of the column and an integer sum cast it to 64 bits:
  0.8 GB of scratch per column at 1e8 rows.
- **Target shape.** `group.rs` runs such a query through the aggregate kernel as a single group
  with no row codes (`whole`), so a sum reads the column's chunks in place.
- **Impact.** The benchmark's checksum (`tools/bench/internal/table/ry.R`) asks the engine for
  the sums instead of converting an answer's columns to R vectors, which copied the answer.

## DataFrame expression operands are checked against the engine's own typing

**Status:** built.

- **Previous shape.** `frame_expression` in `check.rs` computed each expression's column type but
  never looked at its operands, so `Expr.col("region") * 2` on a `character[]` column passed the
  checker and failed only when the query ran.
- **Target shape.** The same walk checks operands with the rules of the engine's `Typer`
  (`crates/table/src/query.rs`): arithmetic, unary `-` and the numeric aggregations refuse text
  (`character` or `factor`), `&`/`|`/`!` refuse text, and the test of `Expr.if_else` must be
  `logical`. The class comes from `join_class`, so a type outside logical/number/text (unknown,
  `Any`) stays silent. Two error kinds, `ExpressionOperand` and `ExpressionComparison`, name the
  column when the operand is a bare `Expr.col("x")`.
- **Deliberately stricter than the engine.** The engine compares an `integer` or `logical` with a
  string by formatting the number, as R does; the checker reports text against `integer` or
  `double` all the same, because with a known schema that pairing is almost always the wrong
  column. Text against `logical` stays allowed, so `Expr.col("x") == NA` is not flagged.
- **Impact.** No new state: the checks run inside the existing refinement of a verb call, which is
  per item, so incremental analysis is unchanged.

## Strict mode reports a DataFrame query over unknown columns

**Status:** built.

- **Previous shape.** A `DataFrame` whose columns are unknown (a computed `read_csv` path, a bare
  `DataFrame` parameter, a verb argument the checker cannot read) is a known type, not `Unknown`,
  so strict mode let every `Expr.col("x")` on it through unchecked, and "a clean strict run checks
  every column" did not hold.
- **Target shape.** `note_unchecked_columns` in `check.rs` records a strict origin
  (`StrictOriginKind::UnknownColumns`) on the subject of a verb that names a column, through a
  string argument or a `Expr.col("x")` anywhere inside one, when the subject's columns are unknown. A
  verb whose chain already has an earlier step over unknown columns that names a column is
  skipped, so a chain is reported once, at its first step that names a column; steps that name
  none (`head`, `lazy`) are looked through. The message suggests the annotation with the first column the query names.
- **Why at the query, not at the read.** The unknown schema has many origins (readers,
  parameters, pivots, unreadable arguments), and a frame nobody queries by column needs no schema.
  Reporting the first unchecked query is precise and always actionable; an annotation on the
  binding gives the frame columns, so the origin disappears rather than needing to be cleared.
- **`read_csv` has its own reader typing.** `read_csv_type` matches `read_csv`'s own formals
  (`path`, `strings_as_factors`) instead of borrowing `fread_type`, whose argument whitelist made
  `strings_as_factors = TRUE` fall back to unknown columns. Both share `sniffed_columns`, and the
  file read goes through the per-path `data_file_layout` query, so a data-file edit re-checks only
  the reads of that file (`a_data_file_edit_rechecks_only_the_reads_of_that_file`).
- **Impact.** No new state; the check reads recorded types inside the existing per-item
  refinement.

## CSV files are read in released slices straight into the final column buffers

**Status:** built.

- **Previous shape.** `read_csv_with` handed the whole mmapped file to one Polars parse, then
  converted each column to its R type as a second copy.
  At 1e8 group-by rows the file (5.2 GB), the parsed frame and the converted columns were all
  resident together: the load peaked at 9.9 GB and the run did not fit in 15 GB.
- **Target shape.** `read_bytes` (`crates/table/src/io.rs`) parses the file in about 256 MB slices
  that end on a line break outside quotes (`slices`, which runs the quote state machine
  `Quoting` over each stretch in parallel from all three start states and chains them), appends
  each slice's columns into one `Builder` per column (a `Vec<i32>`, `Vec<f64>` or `Vec<u32>` of
  factor codes plus a validity bitmap, reserved from the first slice's bytes per row), and
  `madvise(DONTNEED)`s the parsed slice. A factor column's slices are interned into one
  `factor::Interner` (see "Factors are ry's own codes plus one level table, outside Polars").
- **Fallbacks.** A Polars parse error on
  a slice retries the whole file as text, so a row a slice boundary breaks never changes the
  answer. `check_csv_input` (the fuzz target) compares a 16-byte sliced read with a whole read.
- **Impact.** 1e8 load peak 5.6 GB (was 9.9 GB). No duplicated state: the builders are the final
  buffers. The rest of the 1e8 peak is q10, which has nearly one group per row, so `aggregate` and
  `hashed` in `group.rs` drop each per-row and per-group buffer as soon as the next step no longer
  needs it; the whole group-by run then peaks at 13.4 GB from R and 12.6 GB from Rust, against
  data.table's 13.7 GB.

## The Rust table benchmark runs on libR's allocator and the same lazy plans as the R path

**Status:** built.

- **Previous shape.** `db_benchmark` used glibc malloc and answered joins through the data.table
  `x[i]` IR, while `ry run` used libR's mimalloc and lazy `Plan`s, so "ry from Rust" (9.2 s at
  1e7) lost to "ry from R" (6.3 s) for reasons that had nothing to do with the API.
- **Target shape.** `benches/allocator.rs` installs the allocator libR declares
  (`heap::large::Mapped` over mimalloc, eager commit off, purge delay 1e9 ms) as the global
  allocator, and the joins run the same `Plan`s as `ry.R`. A block of a megabyte or more gets its
  own mapping and returns to the kernel when freed, as under `ry run`, so the driver's peak
  resident memory is the engine's and not that of freed columns mimalloc keeps (12.0 GB from
  Rust against 11.6 GB from R on the 1e8 joins). The driver sums an answer's integer columns
  value by value, because a cast to doubles holds 0.8 GB beside a 1e8-row answer.

## Table operations map their large blocks on huge pages

**Status:** built.

- **Previous shape.** `heap::large::Mapped` gave every block of 1 MB or more a fresh anonymous
  mapping on 4 KiB pages. A table query allocates buffers as long as its columns and frees them
  when it returns, so every query paid a page fault per 4 KiB of every buffer: from the Rust
  driver at 1e7 rows in a build without LTO, group-by q1 took 0.106 s against 0.024 s on
  mimalloc's reused pages, and the 1e8 group-by took 32.2 s from R.
- **Target shape.** A table operation holds a `heap::large::huge_pages` guard, and while any
  guard lives, `Mapped::alloc` and `Mapped::realloc` ask for transparent huge pages
  (`MADV_HUGEPAGE`) on a mapping of 2 MiB or more. The guards are counted in one atomic for the
  process, because an operation's buffers are mapped on the worker threads. The interpreter
  takes the guard in every table builtin (`table_work!` in `crates/frames/src/frame.rs` wraps
  the entries of both builtin tables) and in `frame::run`, which serves `[` on a frame; the
  benchmark drivers take it for their whole run. Outside a guard a block gets small pages, so a
  process that runs no table operation issues the same `madvise` calls as before the advice
  existed. The advice is a no-op on a kernel without transparent huge pages. `MappedPool`, which
  serves large R vectors, never asks: it resizes kept mappings, and the huge-page measurement on
  the subset loop ("Collections run on GNU R's schedule") stands for it.
- **Rejected: advising every large block by its size alone.** The size cannot tell a buffer
  that is filled in full from a block that is reserved and filled in part, which is what the
  interpreter's large blocks are. Each commits its last, partly used 2 MiB in full: a hash table
  that had doubled to 3.8 MB and 7.6 MB raised the peak of a script that builds many objects by
  0.86 MB, a vector that grew from 1 to 4 MiB raised a loop over a data.table by 0.5 MB, and the
  bytecode register file (16 MiB, touched as deep as calls nest) raised every script by 2 MB.
  An opt-out per block (`MADV_NOHUGEPAGE` on the register file) fixed only the block that had
  one.
- **Rejected: advising only blocks of 32 MiB or more.** It cost the 1e7-row joins 15 to 40%
  (q3 0.14 s against 0.10 s), because their buffers of 9 to 20 MB went back to a fault per
  4 KiB, and a fault costs about 1.4 µs on the measuring machine (11 ms for 32 MiB).
- **Rejected: a pool of freed mappings inside `Mapped`.** It would reuse touched pages, which is
  faster still on a query whose buffers are as large as the table (group-by q10 at 1e7 in the
  same build: 0.43 s on reused pages against 0.73 s on fresh huge pages), but it keeps memory
  resident between queries, and the 1e8 joins have about 3 GB to spare. It stays in
  `backlog.md`.
- **Impact.** Group-by q1 at 1e7 rows in that build is 0.028 s (0.014 s in a release build), and
  the 1e8 group-by takes 15.5 to 17.9 s from R over three runs against data.table's 59.3 to
  65.3 s, at the same 8.5 GB peak as on small pages. Scoping the advice costs the queries
  nothing: the 1e7 joins from R took the same time as with size-only advice over three
  back-to-back runs of both builds. A script that runs no table operation issues the R heap's
  two `madvise(MADV_HUGEPAGE)` calls and none from the allocator, as it did before the advice
  existed.
- **The gain varies with how fast the kernel finds free 2 MiB blocks.** On one build the 1e8
  joins took 8.1 s from R in one run and 11.1 s in another (9.2 s and 11.3 s from Rust), and a
  Rust-driver run of the slower kind counted about 1,100 direct compaction stalls
  (`compact_stall` in `/proc/vmstat`) with almost no fallback to small pages. A pool of freed
  mappings would not depend on it (`backlog.md`, "A query's large buffers are faulted in afresh
  each time").

## Group-bys and joins run on ry's kernels, not Polars' streaming engine

**Status:** built.

- **Question.** Polars' streaming engine runs a plan in morsels and can spill, so it was the
  candidate for fitting db-benchmark's 1e8 joins and group-by in 15 GB without ry's own kernels
  (`crates/table/src/group.rs`, `join.rs`) and without the allocator work.
- **Measurement.** `db_benchmark --polars-streaming`, built with `--features polars-streaming`
  (`crates/table/benches/streaming.rs`), runs each question as a plain Polars plan on the
  streaming engine over the tables ry's reader built, with factor columns replaced by their
  codes, in the same process and allocator as the kernels. Faster of two runs, seconds,
  4 threads, 15 GB. The two columns were measured back to back; the design page's table is a
  separate run, and runs of the kernels differ by up to 35% ("Table operations map their
  large blocks on huge pages"):

  | 1e8 rows | ry kernels | Polars 0.55 streaming |
  | --- | ---: | ---: |
  | group-by q1 | 0.17 | 0.11 |
  | group-by q2 | 0.18 | 0.91 |
  | group-by q3 | 1.25 | 1.47 |
  | group-by q4 | 0.82 | 0.22 |
  | group-by q5 | 1.32 | 1.26 |
  | group-by q6 | 0.86 | 2.59 |
  | group-by q7 | 0.62 | 1.80 |
  | group-by q8 | 2.08 | 7.82 |
  | group-by q9 | 0.40 | 1.44 |
  | group-by q10 | 6.32 | out of memory |
  | group-by peak | 8.5 GB | over 14.2 GB |
  | join q1 | 0.71 | 0.64 |
  | join q2 | 0.84 | 1.02 |
  | join q3 | 1.29 | 1.07 |
  | join q4 | 0.76 | 0.80 |
  | join q5 | 5.60 | out of memory |
  | join peak | 11.9 GB | over 14.2 GB |

  At 1e7 rows, where everything fits, streaming peaks at 2.10 GB against 1.15 GB on the group-by
  and 2.64 GB against 1.77 GB on the joins, and takes 1.80 s against 1.28 s and 1.47 s against
  0.67 s in total.
- **Decision.** The kernels stay. Streaming runs out of memory on exactly the two questions the
  15 GB budget is about (the group-by with nearly one group per row and the join of two 1e8-row
  tables), and it is slower on eight of the thirteen questions it finishes at 1e8, by 3.6 to 5.1
  times on group-by q2, q8 and q9.
- **What streaming does better**, kept as work for the kernels (`backlog.md`, "Open: DataFrame"):
  group-by q4 (three double means over 100 groups), where ry adds doubles in row order as
  data.table does and Polars sums per morsel, and group-by q1 and join q1 and q3 by 10 to 35%.
  Group-by q5 is level (1.26 s against 1.32 s).
- **Impact.** No engine switch and no second execution path; the `streaming` feature of Polars is
  a dev-dependency of `crates/table` for the bench driver alone.

## The data.table migration is one engine in `ide`, read off the checker's types

**Status:** built.

- **Where it lives.** `crates/ide/src/migrate.rs` (`data_table_rewrites`, `apply_data_table_rewrites`) is a pure read of the analysis database, like every editor feature. `code_actions` offers each rewrite that overlaps the viewport plus one whole-file action, and `ry migrate data-table` (`cli::migrate_data_table`) analyzes the requested files exactly as `ry check` does and applies the same list, so the editor and the CLI cannot disagree. It is not a lint fix: a lint's fix is local to its diagnostic and needs no types, while a rewrite needs the target's type and the columns it knows.
- **What counts as a table.** A query `x[...]` is rewritten only when `item_check`'s type of `x` is a `Frame` (data.table or DataFrame) or the `data.table` named type, so a vector index is never touched.
- **Column or variable.** With the frame's columns known, a bare name is a column exactly when it is one of them; data.table also looks columns up first, so a same-named variable loses there too. With unknown columns, a name is a column unless it resolves to a local binding (`item_naming().resolutions`), is a project global name (`project_global_names`), is a base constant (`T`, `F`, `pi`, `letters`, ...), or is one of data.table's own symbols. A guess cannot be wrong silently in the other direction: a name taken for a column that is not one makes the DataFrame query fail with a missing-column error at run time.
- **Nothing approximate.** Every unsupported shape returns `None` and produces no rewrite. Updates in place (`:=`, `setorder`, `setDT`) are rewritten only as top-level statements outside any function, as `x <- x |> ...`, because inside a function the caller would stop seeing the change. `dt[i, col := v]` needs known columns: an existing column is rewritten only for a literal of its own type, as `Expr.if_else(test & !Expr.is_null(test), v, Expr.col("col"))`, since data.table leaves a row whose test is `NA` unchanged. A keyed join (`setkey` then `X[Y]`) is skipped because it needs statement order.
- **A rewritten table never escapes to unrewritten code.** Every rewrite turns a data.table into a DataFrame. `[.DataFrame` still answers `x[i, j, by]` and `:=`, but data.table's own functions refuse one (`setorder(df, -x)` stops with "x must be a data.frame or data.table"), and so does any package code that checks `is.data.table`, so the whole-file migration (`DataTableRewrite.replacement`) keeps a form only where its table flows into other kept rewrites, into `$`, `[[` or a shape function (`nrow`, `names`, ...), or is discarded. A query that is its function's return value is wrapped in `data.table::as.data.table()`, so a function's interface keeps its classes (parameters already enter through `as.DataFrame()`). Every other form gets `replacement: None`; dropping one can expose another, so `data_table_rewrites` iterates to a fixpoint (`fate`, `uses_are_safe`). Uses are matched by name across the whole scope, which can only leave more forms alone. A single code action (`standalone`) is user-directed and stays offered. Found by `tools/bench/tooling/migrate_fuzz.py`: the previous engine rewrote the `data.table()` constructors in the benchmark templates while their callers stayed data.table code.
- **Accepted differences.** DataFrame aggregations skip `NA` (so `na.rm = TRUE` is dropped and `sum(x)` without it no longer gives `NA`), `DataFrame.sort` puts `NA` last where `setorder` puts it first, and a join lists the left table's columns first. The guide's own mapping makes the same choices; `docs/.../reference/from-data-table.md` states them.

## Dates, windows, ranks, cross joins and pivots in the table engine

**Status:** built.

- **Date is a column type.** `ColumnType::Date` maps to Polars' `Date` (whole days since 1970-01-01) and to an R double with class `"Date"` both ways (`table::convert::RVector::Date`, `libr::frame_altrep::date_vector`, `frames::frame::literal`). A fractional R date floors to its day, as `as.Date` printing does. Typing follows R's own rules for days: date ± number is a date, date - date is `integer[]` days, dates compare only with dates, and any other operator on a date is a `DateOperation` error (`check.rs`, `date_operation`). R itself would give a `difftime` for date - date; the engine has no duration column, so the integer day count is the deliberate shape.
- **`Ops.Expr` beats `Ops.Date`.** `Expr.col("day") >= as.Date(...)` has two S3 `Ops` methods, which R refuses as incompatible. `chooseOpsMethod.Expr` returns `TRUE` (R >= 4.3 protocol, honored by `interp::dispatch`), so the expression method always wins; the stub's `Ops.Expr` accepts `Date` on either side.
- **Windows and ranks lower to Polars.** `Expr.over(e, keys)` is `Expr::Window` and `Expr.rank` is `Expr::Rank` (ordinal, ties broken by row, `NA` last). The fast kernels decline both, so they run in Polars; the checker types `over` as per-row with its argument's type and `rank` as `integer[]`. The ordinal row number is `is_null().cum_count()`, because a constant `TRUE` mask is folded by Polars into a scalar of length one.
- **Cross join is a `How`.** `How::Cross` takes no keys (`check_join_keys` rejects any), renames clashing right columns with the suffix, and keeps left-major order; the checker's `join_type` treats it as a join with empty keys.
- **Pivot is eager.** `DataFrame.pivot` and `LazyFrame.pivot` collect first and run `table::ops::dcast`, because the output columns depend on the data, so no lazy plan can know its schema. The checker types the result as an unrefined `DataFrame` for the same reason.
- **A pivot addresses cells by dense ids.** `dcast_dense` in `crates/table/src/ops.rs` turns the row keys and the spread column into dense ids from their order digits (the same per-column sort codes `sort_frame` uses), so each cell is `row_id * labels + label_id` and one pass fills or aggregates it. The earlier path grouped by every row key and the label and then looked each group up by its label text, which took 4.5 s for a 1e7-row pivot against 0.7 s now. The grouped path stays as the fallback for keys whose digits do not fit a u64 or a cell count past `i32`.
- **Pushdown respects order and groups.** A filter that reads a window, rank or aggregate depends on which rows it sees, so it moves only below a sort, and a filter that also reads row order (`first`, `last`, `head`, `rank`: `lazy::reads_order`) does not move below a sort either. Every pass that needs the columns an expression reads goes through `query::column_references`, which includes window keys, so projection pushdown never drops a key.

## A `NULL` local inside a query of unknown columns is the column

**Status:** built.

- **Before.** `time <- NULL` (the R CMD check "no visible binding" idiom) followed by
  `dt[...][order(-time)]` read the lexical `NULL` whenever the columns were unknown, and `-time`
  was reported (data.table's own test runner).
- **Shape.** `read_column` types a resolved `NULL` local as an `Unknown` masked column when the
  scope's columns are unknown. A plain `[` on a subject that may be a data.table (declared, with
  data.table syntax, or `Unknown`/`Any`/an open variable) counts `unseen_frames`, and `infer_read`
  turns a `NULL` local read into `Unknown` there. A column scope was not pushed for the plain case
  because it masks every read, which hid a true unresolved name in `m[not_a_column]`.

## String keys sort by rank, and merge pairs come from sorted codes

**Status:** built.

- **Previous shape.** `order` and `sort` on character vectors compared the strings themselves
  (a comparison sort over `Vec<String>`-like keys, or text hashed into a map), the radix sort
  gathered `(key, row)` pairs for every bucket, `paste(collapse =)` built each row as a `String`
  and joined them, `make.unique` kept every name as a Rust `String` in a hash set, and the
  `merge` kernel kept an `FxHashMap` of `Vec` per key. Each held a second copy of data the heap
  already had, so peaks were 1.17 to 1.37 times GNU R's.
- **Target shape.** A character key becomes `Column::Ranks`: one `u32` per row, the rank of its
  text among the vector's distinct strings, 0 for `NA` (`string_ranks` in
  `crates/interp/src/sort.rs`). The distinct strings are found by data.table's trick: each
  CHARSXP's truelength temporarily holds a negated slot index, checked against the list of slots
  so a stale value is never trusted, and restored afterwards. Strings are interned, so a CHARSXP
  is its text per encoding. Only the distinct texts are sorted, by collation for `order` and
  `sort` and by bytes for the radix method, and the ranks then go through the integer radix sort.
  The radix sort scatters rows only, and gathers `(key, row)` pairs only for buckets of at most
  65,536 rows, which fit the cache. `make.unique` with an ASCII separator and ASCII names marks
  the names the same way and builds candidates as CHARSXPs; `paste(collapse =)` writes all rows
  into one byte buffer and makes one CHARSXP. `merge`'s matches are found by binary search in
  sorted `(code, row)` pairs, sized by a counting pass and written straight into the result.
- **Impact.** At 1e6 rows every base kernel's peak is within 1.13 times GNU R's (`sort(chr)` 1.04,
  `order(chr)` 1.03 at 0.11 s against 1.9 s, `paste(collapse =)` 0.89, `merge` 0.89 at 0.8 s
  against 2.2 s, `unsplit` 1.13), and `make.unique` on 1e6 names fell from 227 MB and 0.9 s to
  127 MB and 0.45 s (GNU R 92 MB, 1.1 s). The truelength marks survive allocation, because the
  collector never reads truelength and the marked strings stay reachable from the vector being
  read, but every exit, including an error, must restore them.

## package:ry attaches no bare name another package exports

**Status:** built.

- **Previous shape.** `package:ry` attached bare `setkey`, `setkeyv`, `key` and `copy` for DataFrames. A hook copied them into `package:data.table` so they could delegate for data.tables; once data.table became an ordinary package with no special case, the hook went, and `library(data.table)` printed "The following objects are masked from 'package:ry'", which GNU R does not print. Six oracle cases failed on that one line.
- **Why bare names mask.** `package:ry` sits at position 2, so any package attached later that exports the same name reports the mask, and code that calls the bare name gets whichever was attached last.
- **Target shape.** Every DataFrame function is qualified: `DataFrame.set_key(df, cols)` (one function for both `setkey` forms, taking a character vector), `DataFrame.key(df)` and `DataFrame.copy(df)`. Column expressions are `Expr.col(name)` and constants `Expr.lit(value)`, beside `Expr.sum` and the other expression functions: a bare `col` would mask base R's `col`, and `col` and `lit` build expressions, so they belong under `Expr`. The only bare function is the `DataFrame` constructor, plus the `as.*`/`is.*` conversions; everything else is under `DataFrame.`, `Expr.`, `GroupBy.` or `LazyFrame.`, and no name is exempt.
- **Enforced** by `no_shipped_package_exports_a_library_name` in `crates/interp/tests/test_standard_library.rs`, which checks the standard library and `package:ry`'s functions against every `types/*.exports` manifest and fails when a package in its `MUST_NOT_MASK` list (base R, the 15 recommended packages and the benchmark dependency set) has none. The manifests are vendored so CI needs no GNU R: `scripts/export-manifests.R` records them from an installed library, or with `EXPORT_SOURCES` from a package's NAMESPACE file when it needs a system library or vendor SDK to install (Rglpk, Rblpapi).
