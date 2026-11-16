# Decisions: DataFrame and the table engine

The built-in DataFrame, its kernels, CSV reading, and the data.table boundary.

## The built-in frame

**Status:** built, except the name `frame` and its class, which "The DataFrame API is Polars', and ry's own types are UpperCamelCase" supersedes.

The table is a Polars `DataFrame` held by the `table` crate; this entry holds the semantics of `x[i, j, by]` and `:=` on it.

- **`:=` has value semantics in the checker** (aliases keep the old type) and is copy-on-write at run time: in place on an unshared table, else after `Table::fork`, which copies only the column list because Arrow buffers are shared. data.table's own `:=` keeps reference semantics.
- **`:=` writes only a slot the current scope already owns** (`naming.rs`, the `Index` arm), because naming runs before types and cannot tell a frame from a `data.table`; minting a local broke goto-definition on a global `data.table` updated in a function. A global frame's update refines the non-local name (`non_local_slots`).
- **`:=` into an existing column keeps its type unless the value replaces it whole** (`assigned_type` in `query.rs`), as data.table coerces to the column. The checker treats only a literal as one value (`is_literal`), because a scalar-like type is no length guarantee. A string written into a factor keeps the factor and appends the new labels as levels in order of appearance, as data.table does.
- **One query IR** (`crates/table/src/query.rs`) serves the interpreter, the checker's `result_type` and the tests. Lowering imposes R semantics on Polars (integer overflow to `NA`, NaN comparisons `NA`, `%/%` floors, `%%` takes the divisor's sign, `cor` `NA` where Polars gives `NaN`, `max`/`min` NaN when one is present, `!`/`&`/`|` treat a nonzero number as `TRUE`, aggregates propagate `NA` unless `na.rm` through a `null_count` guard that costs nothing on null-free input).
- **Integer `sum` is a double exactly when a total overflows** (R 4.3, data.table's gsum); totals stay 64-bit until known (`narrow_sums`, `staged`), and a grouped `:=` becomes double if any group overflows, where data.table takes the first group's type. A new `:=` column nesting a sum is computed both as written and with each sum over doubles (keeping the expression's type, so `sum(v) > 0L` stays logical), and hidden probe sums pick the version (`assign` in `query.rs`).
- **data.table's layout is the contract for joins and grouping:** rows follow `i`, `x`'s columns first, clashes prefixed `i.`, NA keys join each other (`join_nulls`), `by` keeps first-appearance order and `keyby` sorts (`group_by_stable`). Join columns of different types meet through a common type (two factors as strings) and are cast back to `i`'s type. A keyed join without `on` matches x's key positionally against i's key, or i's leading columns when i has none (so `Join.other` is a `Table`, carrying the key). An equi join keeps i's order then x's (`MaintainOrderJoin::LeftRight`). A rolling join marks x's rows with a hidden column so an inner rolling join keeps matches whose values are NA. `mult = "first" | "last"` keeps one match per row of `i`; `"all"` is the default.
- **ALTREP column exposure lives in `libr` (`crates/libr/src/frame_altrep.rs`),** which links Polars anyway; in `table` it would make the engine depend on the runtime. The ALTREP object is the only R-layout copy of a column: `data1` owns the Arrow array, `data2` holds R's layout once something needs it, and a column R never materialized comes back from `import_column` as the same buffer. NA is Arrow validity and NaN a value, so a column without nulls is zero-copy.
- **Native kernels run below Polars, with plans as fallback and reference** (`kernel.rs`, `group.rs`, `join.rs`), because Polars pays for planning, overflow widening, whole-column rebuilds for a partial `:=` and nested `take`. The fuzz in `crates/table/tests/test_table.rs` checks every kernel against `query::execute_with_plans_only`. `FUZZ_ITERS` raises the iteration count. Every column is one contiguous chunk (`normalize`, `put_column`, `set_key`, `from_normalized`), because kernels read one slice and the CSV reader returns many chunks; without the invariant every query recopied its keys and inputs (a third of an integer-keyed group-by's time).
- **Grouping codes keys densely as mixed-radix digits of one `u64`** (factors by code, integers by `value - min`, logicals by two values); a range wider than the rows (or 2^20) hash-partitions, because dense states cost memory per possible code. Dense runs split across threads only when the rows outnumber every thread's states, and the equi-join kernel bounds its buckets the same way.
- **Median counting-sorts entries by slot and selects each group's middle in place;** a valid `NaN` without `na.rm` falls back to the plan, whose NaN ordering matches R's. Join output gathers column by column (`gather_column`), factors by code; the rolling join searches integer and double times natively after a parallel sortedness check.
- **Moments use Welford with a symmetric co-moment and Chan's merge,** so `cor(a, b)` equals `cor(b, a)` bit for bit; the fuzz compares doubles to 1e-6.
- **Non-equi joins and `.EACHI` follow data.table except naming:** `join_layout` is the one description of output columns, and repeated conditions on one column give `t.1`, `t.2` where data.table writes `t, t`; a name that still repeats takes the next free numeric suffix, as `make.unique` would. An ordering compares numbers only, and a rolling join must be all `==`. NA and NaN match only the same missing value, and only under `<=`, `>=` or `==`. The kernel (`non_equi` in `join.rs`) sorts each equality group's rows of `x` by the first ordering column and narrows each row of `i` by binary search. `.EACHI` reruns as a join grouped by the row of `i`, so `j` sees an unmatched row as one NA row with `.N` zero, and an aggregate over an `i` column alone sees its one value (`single_value`, with a hidden `FIRST_OF_ROW` mask where `na.rm` must see one value). Prefixed join columns become ordinary copies before the join (`prefixed_copies`). Under `.EACHI` the checker types `i`'s columns as scalars inside `j` (each row of `i` runs `j` once) while `x`'s stay vectors; the groups are the compared columns in condition order, matching `each_i`.
- **Warnings are data in `Executed`,** raised after the query succeeds, so a failed query raises none and kernels and plans compare on them. Three exist, where data.table raises its own (wording is ry's): an overflowing integer `sum` selected or assigned by a grouped query (none for ungrouped, nested or `.EACHI` sums); a `:=` whose column type loses part of the value (a narrowing `:=` skips the kernel, because detecting the loss needs the value before conversion); and `Parsed`, strings written into a logical or numeric column, decided from types alone and parsed as `as.logical`/`as.integer` (through a double, so `"3.7"` is 3)/`as.double` would. NaN becomes NA silently.
- **The checker mirrors `join_layout` in `join_columns`; it cannot call it,** because `semantics` must not depend on Polars. The `frame_joins` fixtures pin both against data.table. Condition compatibility follows the engine's `comparison_types`: numbers with numbers, strings with strings, logicals with logicals, and never an ordering on strings. A factor column is `Any` to the checker, so it is not checked.
- **A lossy `:=` is a type error, though data.table only warns,** because the result is almost always a bug and `as.*()` states intent. A number into a string column converts without loss and stays accepted, as R code relies on it.
- **`lapply(.SD, f)` is typed once per column** (`subset_apply`), because `.SD` through `lapply` is a list of the union of column types (a false error for any `f` that fits each column). An overloaded `f` passed by name is called on each column through `overloaded_call`, because an overload set has no single function type. A finding every column causes is reported once; one only some cause names the column.
- **A result repeating a column name has unknown columns,** because a record cannot hold two fields of one name.

## Frames as runtime values

**Status:** built, except the class name `"frame"` and `ry` as the standard library's home, which "The DataFrame API is Polars', and ry's own types are UpperCamelCase" and "The standard library is the `std` namespace, with an optional prelude" supersede.

- **A frame is an `EXTPTRSXP` owning a `Box<table::Table>`** (`crates/frames/src/frame.rs`), dropped by a C finalizer. The table is the only copy of the data; a column becomes an R vector when R reads it. It is not a `data.frame`, because a data.frame's list layout and row names do not hold; the checker agrees (fails a `data.frame` parameter, passes a `list` one).
- **`x[i, j, by]` on a frame never reaches the `[` primitive:** `dispatch::dispatch_or_eval` evaluates only the subject and hands the unevaluated call to `frame::run`, because `[.frame` would be too late when `i`, `j` and `by` name columns. The `[` method exists for evaluated calls (`do.call`, `Map`). The bytecode compiles `[` as a generic call, so loops that subset run the same path.
- **Any other object subject of `[` dispatches lazily,** as GNU R's `DispatchOrEval` does, so `[.data.table` receives expressions.
- **libr's option reads (`Rf_GetOption1`, `Rf_GetOptionWidth`, `Rf_GetOptionDigits`) read base's `.Options`,** the one store `options()` uses (`option_value` in `crates/libr/src/environments.rs`). With two stores C code saw no option R code set, and data.table failed with "Has getOption('datatable.alloccol') somehow become unset?".
- **Lowering first, R second.** Parts of `i`, `j`, `by` that do not lower run as R in a data mask binding only the columns the expression mentions plus `.N`, `.SD`, `.I`, `.GRP`, `..x` and `.`; grouped R code uses Polars for the grouping and evaluates once per group.
- **Copy-on-modify:** `:=` updates in place when the named variable holds the only reference, else forks and rebinds that variable where it lives (searching enclosing scopes). The only reference reads as a count of two, because the frame's finalizer counts one. `DataFrame.set_key` always forks.
- **S3 methods for other packages' generics are registered in the generic's table,** as `S3method()` directives are; `as.data.table`'s method is registered from a data.table `onLoad` hook, because data.table is not loaded at startup. The checker's stubs `types/ry.Rtypes` and `types/ry.exports` are kept equal to what the runtime binds by `test_standard_library.rs`.

## data.table runs on its own code; the `[` bridge is removed

**Status:** built, except `ry::setkey`, `setkeyv`, `key` and `copy`, which "package:ry attaches no bare name another package exports" supersedes.

- **A `[` on a data.table always runs data.table's code;** no engine bridge, dispatch hook or masking hook exists. A bridge that declines whenever data.table would behave its own way must predict an open-ended set (data.table's wording, over-allocation and index state in its C, every query shape), and a seeded differential (7,500 queries over five seeds, bridge on against `options(ry.datatable = FALSE)`, compared by `identical()` on values, attributes, both tables afterwards, output and conditions) found 74 of the 1,028 it answered (7.2%) wrong: lost warnings (`.SDcols` unused by `j`, `max()` of an empty group, `>` on a factor), a missing cartesian-product error, a `max` over characters data.table rejects, NA bit patterns. The generator covered only part of the query surface, so more causes would follow. The differential harness went with the bridge. It was also a data.table special case, which the user directive rules out.
- **Cost accepted:** grouped queries on a data.table lose the engine; the DataFrame is the fast table, chosen for new code. The `as.data.table` method for DataFrame stays, because it converts a DataFrame. The `datatable__*` oracle cases check data.table's own code under ry.

## Delimited files: one sniffer for the reader and the checker

**Status:** built, except "fread returns a frame", which "data.table's helpers run data.table's own C, and the C API is what makes them fast" supersedes.

- **`crates/sniff` decides a delimited file's layout with fread's rules** (separator, header, names, logical/integer/double/character), from the head plus jump points. It is the single source of truth: `table::io::read_csv` parses with it, and the hosts sniff each project `.csv`/`.tsv` head (64 KiB, `sniff_head`) into the `DataFiles` salsa input, read per path through `data_file_layout`, so editing a data file invalidates only the checks that read it. A column whose sample missed a wider value is reread from all its values, so the reader never gives a narrower type than the data holds.
- **The CLI walks data files ignoring `.gitignore`,** since data directories are commonly ignored. The checker resolves a data path against the project root only, because `SourceFile` carries no path.
- **The checker refines reader calls after the ordinary call checked their arguments** (`refine_table_call` in `check.rs`), reading only recorded argument types, so nothing infers twice; `setnames` and `setcolorder` reuse the `:=` slot-write model (`naming::record_column_update`). `data.table()`, `as.data.table` and `setDT` keep the `data.table` nominal.
- **`fwrite`-compatible writer:** shortest round-trip doubles (`1`, not `1.0`), `TRUE`/`FALSE`, `NA` as an empty field, minimal quoting; batches format in parallel and write in order.
- **Known gaps:** integer64 reads as double, dates as character, `dec = ","` and `strip.white` are unsupported, and a ragged line is an error rather than fread's fill.

## The table engine's sort and distinct kernel

**Status:** built.

- **One kernel serves every ordering operation,** so all agree on NA placement and ties. `ops::order_digits` codes each key column as one `u64` per row whose unsigned order is the sort order: `NA` 0 and `NaN` 1, both first in either direction as data.table places them; integers offset from their minimum; doubles through `ordered_bits` so `-0` ties `0`; factors by code; strings by byte-order rank (`string_ranks`). `sorted_rows` packs digits and row number into a `u64` or `u128`, so every key is distinct and the sort is stable; `radix_sort` does up to three LSD passes, or one MSD pass on the top 16 bits then an in-cache sort per bucket. Keys wider than 96 bits fall back to Polars.
- **Rejected: Polars' multi-column sort and `unique_stable`,** which compare row by row through the column types, cannot match data.table's NA, NaN and `-0` rules without extra passes, and lost to data.table at 1e7 rows.
- **Distinct rows:** keys of 24 bits or fewer skip sorting (`uniqueN` sets bits in a shared atomic bitmap, `unique` keeps a first-row table per task); wider keys sort and compare neighbours.
- **String ranks:** each thread numbers its slice's strings in a hash map keyed by the 16-byte view itself for strings of 12 bytes or fewer (no string read), with a small direct-mapped cache in front while the slice has few distinct strings; the distinct strings sort once by `(first 16 bytes big-endian, string)`, which is byte order with mostly integer comparisons.
- **A gather shares string data buffers,** so reordering moves no string bytes.
- Pinned by `fuzz_sort_unique_and_runs_against_a_reference` in `crates/table/tests/test_table.rs`, against a reference comparator over random NA, NaN, `-0`, infinities, factors and long shared-prefix strings.

## The DataFrame API is Polars', and ry's own types are UpperCamelCase

**Status:** built, except routing plain data.table queries to the engine, which "data.table runs on its own code; the `[` bridge is removed" supersedes.

- **Every type ry adds is UpperCamelCase** (`DataFrame`, `LazyFrame`, `GroupBy`, `Expr`, `Record{...}`, `Tuple{...}`, `Result`, `List`, ...) and R's own types stay lowercase, so case is the visible line between them. No lowercase alias was kept: the old names had no users outside this repository, and an alias adds a second spelling to every renderer and fixture.
- **API: Polars' verbs over expressions as qualified functions used with `|>`** (`DataFrame.select`, `filter`, `group_by` + `GroupBy.agg`, `join`, `lazy`, the `LazyFrame.*` mirror; `Expr.col`, `Expr.lit`, R operators through `Ops.Expr`). No method dispatch: a qualified name always means one function, so the checker and IDE do a plain name lookup. A string names a column wherever a verb takes an expression.
- **One plan for eager and lazy:** every verb appends a `table::lazy::Step`, and an eager verb collects at once. The optimizer pushes filters below `with_columns` and projections and into a scan, prunes scan columns, and turns a leading `head` into a scan row limit; `crates/table/tests/test_lazy.rs` fuzzes that optimizing never changes a result. Errors name the schema of the longest valid prefix (`Plan::valid_schema`).
- **Typing:** `refine_dataframe_call` in `crates/semantics/src/check.rs` refines a verb after the ordinary call checked its arguments against `types/ry.Rtypes`, only when the name reaches the library (`reaches_library`). An argument it cannot read (an `Expr` in a variable) leaves the columns unknown rather than guessing; a project's own `DataFrame.filter` is an ordinary function. `as.data.table` and `as.DataFrame` keep the columns. The refinement reads only the item's recorded types, so incremental analysis is unaffected.
- **Bracket syntax on a DataFrame stays as compatibility,** typing included; the docs teach the verbs.
- **The class is `"DataFrame"` alone;** `as.data.frame` and `as.data.table` methods are registered with `registerS3method` into the generics' namespaces, so they work whatever is attached first.

## Per-group top rows and as-of joins reuse the engine's kernels

**Status:** built.

- **`Expr.head(n)` inside `agg` keeps up to `n` values per group, as rows,** because ry has no list columns (also data.table's layout). The checker tracks an expression's `Shape` (per row, per group, up to `n` per group) in `check.rs` and accepts `Head` only as a whole `agg` item, since `head(v, 2) + 1` would mix shapes. Sort then aggregate fuses into one query that hits the top-k kernel (`top_k_shape` in `group.rs`).
- **`join_asof` is `How::Asof(Strategy)`, lowered to the rolling join** with the left frame as data.table's `i`: `"backward"` is `roll = TRUE`, `"forward"` `roll = -Inf`, `"nearest"` `roll = "nearest"`; `by` columns are leading equality keys. One kernel (`rolling` in `join.rs`) serves both APIs.
- **A grouped median holds `(row, slot)` pairs, one list per run of rows,** reading values from the input during the counting sort, because `(slot, value)` lists concatenated across threads cost 240 MB of fresh memory per 1e7-row query, and faulting it in made group-by q6 from R two to ten times slower than the same kernel from Rust.
- Pinned by `frames.R.test` and `crates/table/tests/test_lazy.rs`.

## data.table's helpers run data.table's own C, and the C API is what makes them fast

**Status:** built, except the `[` bridge, which "data.table runs on its own code; the `[` bridge is removed" supersedes.

- **No routing hook for `fread`, `fwrite`, `setkey`, `rbindlist`, `melt`, `dcast`, `shift` and friends;** they stay data.table's own code, so their output is data.table's by construction. Their speed comes from the C API (`crates/libr/src/vectors.rs`, `memory.rs`): an accessor on a plain, non-ALTREP object of the right type reads directly and enters the error boundary only to raise. Every compiled package gains. With the boundary on every call (`TRUELENGTH`, `STRING_ELT`, `LENGTH`, `INTEGER`, `PROTECT`), data.table's C ran at up to 2.5 times GNU R's time; without it `fread` of the 1e7-row group-by file takes 2.2 to 2.3 s against GNU R's 2.8 to 3.8 s, and `transpose` (the core of `tstrsplit`) 0.062 s against 0.055 to 0.068 s at 1e6 rows.
- **Routing `fread` to the engine was measured slower,** 3.2 to 3.7 s: the reader takes 1.4 s, but converting its columns to R vectors (one CHARSXP per string) costs about 1.9 s. The engine is fastest where data stays inside it (`frame_read_csv` reads the same file in 1.4 s). The numeric helpers are already a `memmove` or one loop in C. A helper moves to the engine only if it wins end to end, conversion included.
- The checker still types `fread`, `rbindlist`, `melt` and `dcast` as returning frames, which is false at run time (`backlog.md`, "The checker and the runtime disagree about data.table's readers").

## Frames and the checker share one CSV reader, and data.table keeps its own

**Status:** built, except reading factors back with `convert::factor_levels`, which "Factors are ry's own codes plus one level table, outside Polars" supersedes.

- **The engine has one CSV reader** (`table::io::read_csv`/`read_csv_with` through `read_bytes` and `read_sliced` in `crates/table/src/io.rs`), and `sniff` decides layout and types for it and the checker. A second, lighter reader was rejected because its inference and quoting could drift from the checker's schema; a faster path is a change inside this reader. Factor columns go through `io::strings_as_factors`.
- data.table's helpers stay on data.table's C (see the entry above); measured at 1e6 and 1e7 rows `fread`, `fwrite`, `rbindlist`, `setkey`, `setorder`, `unique`, `uniqueN`, `melt` and `dcast` all run at or below GNU R's time under ry, each the best of five runs of the call `tools/bench/internal/table/ops.R` makes. `ops.R` itself keeps the faster of two runs, which can show `setorder`, `unique` or `uniqueN` up to 1.25 times slower on either engine; repeated runs do not confirm that.

## A DataFrame column reads back as a plain vector, not an Array

**Status:** built.

- `DataFrame(price = [12.5, 9.99])` checks each column as an Array, stores the plain vector, and `df$price` reads back a plain `double[]`. The rule is in `type-system.mdx` ("DataFrames").
- **Rejected: `Array<T>` reads,** because later steps can put `NA` in a column (a left join's unmatched rows), which would need per-verb tracking and a second representation. `as.Array(df$x)` is the checked way back and fails on `NA`.
- **Rejected: Array only when no step could add `NA`,** because an Array changes arithmetic (no recycling, no integer/double mixing), so adding a join would break unrelated arithmetic downstream.

## The engine's workers return freed memory at large frame boundaries

**Status:** built.

- **libR's allocator is mimalloc, which queues a block freed on another thread until the owner allocates again.** Polars' and rayon's workers allocate columns, the R thread frees them and the pools sit idle, so memory was never returned: a 5e7-row `DataFrame.read_csv(strings_as_factors = TRUE)` left 9 GB resident for a 1.7 GB table, and the db-benchmark group-by script ran out of memory on a 15 GB machine where data.table peaked at 6.4 GB.
- **`table::on_every_worker` runs `mi_collect(true)` on every thread of both pools and the R thread** (`frames::frame::release_worker_memory`) when a frame of at least 2^22 cells is wrapped or finalized, and in `gc()`. The size floor keeps loops over small frames from paying a broadcast each. `mi_collect(true)` drains the cross-thread free queues, so pages become reusable, which bounds the peak; it also returns pages whose purge is scheduled.
- **Trap:** a broadcast waits for every pool thread, and a forked child has none, so whoever implements `fork` (`mcparallel`, `mclapply`) must skip the broadcast in the child.
- **Rejected:** `MIMALLOC_PURGE_DELAY=0` only purges pages already freed (it left 5.9 GB); a per-query arena needs every kernel to allocate through it.

## Group codes wider than 64 bits are ranked, not handed to Polars

**Status:** built.

- `row_codes` (`crates/table/src/group.rs`) replaces the codes so far by their rank among distinct codes before a key column that would overflow a u64. A rank keeps code order (so `keyby` stays sorted) and is below the row count, so any number of key columns fits as long as each fits on top of the row count. Polars' own multi-key group-by took over 12 GB on db-benchmark's six-key question at 5e7 rows (about 2.6e19 codes); ranked, it takes 7 s at a 6.2 GB peak against data.table's 10.5 s.

## Double sums group by code range, one scan per thread

**Status:** built.

- **A double `sum` or `mean` adds each group's values in row order,** as data.table does, so rows cannot be chunked and partial sums merged (rounding would depend on thread count). On one thread, 5e7 rows and 500,000 groups missed cache on almost every row and lost q3 and q5 to data.table. `dense_by_code` (`crates/table/src/group.rs`) gives each thread a contiguous code range; each thread scans every row, keeps its own codes, and states concatenate in code order. The same path serves any dense grouping whose per-thread states would outnumber the rows.
- **Rejected:** scattering rows into code-range partitions first (each partition still reads every cache line, plus a scatter pass) and capping one thread at a cache-sized slot count; from the Rust driver at 5e7, q3 1.42 s scattered against 1.18 s scanned, q4 1.71 s against 1.19 s, and q4 1.42 s with the cap.

## Factors are ry's own codes plus one level table, outside Polars

**Status:** built.

- **A factor is a Polars extension type** (`ry.factor`, feature `dtype-extension`; `crates/table/src/factor.rs`) whose storage is a dense zero-based code column (u8, u16 or u32 by level count) and whose dtype holds one `Arc<Levels>` in R's order. A Polars `Enum` interned every level a second time at about 135 bytes per level, which kept db-benchmark's 1e8 join inputs from fitting in 15 GB. Extension series filter, take, sort and group through their storage, and `to_physical_repr` gives the codes, so kernels need no factor case. Polars refuses to cast extension columns, so every conversion goes through `factor::cast`, `factor::text` or `cast_expr`/`text_expr`.
- **Reader:** a factor column is interned slice by slice into a persistent `Interner` with 256 hash partitions, each owning its labels' views and byte arena. A partition the first slice fills with mostly distinct labels is sized for the whole file from that slice, because growing later copies and rehashes every label with both tables live. `finish` sorts one 16-byte key per level (the label's first 12 bytes big-endian, then its code), re-sorts only runs whose prefix ties, remaps the codes in parallel, and turns the arenas into label buffers without copying. `NA` never enters the levels, so a column holding `NA` is never re-read as text.
- **Joins:** `join::factor_codes` translates one side's codes through `Levels::translation`; the same `Arc` translates by identity.

## A factor's level table is front-coded in byte order, with no hash index

**Status:** built.

- **`Levels` (`crates/table/src/factor.rs`) stores labels sorted by bytes and front-coded in blocks of 16:** a header byte (four bits shared with the previous label, four for the rest, 15 meaning a LEB128 length follows), then the rest bytes; `blocks` holds each block's offset. A reader builds levels in byte order, so a code is a rank; a table whose code order is not byte order (`factor(levels = ...)`, a level union) adds `Order`, two `u32` arrays between code and rank.
- **Why:** per-label views duplicated what sorted order already says (db-benchmark's `id1`..`id100000000` share all but a byte or two with neighbours), and a hash index was derived state that could not be shared between tables. At 1e8 rows `join_x` has 63.2 million `id6` levels, and the views alone were 1.0 GB of a 3.9 GB table.
- **Lookups:** `position` binary-searches the block heads and scans one block; `translation` maps one table's codes into another with a merge of two cursors (factor joins, `recode`). `text` decodes a whole table in parallel when the column has at least a sixteenth as many rows as levels and seeks per row otherwise, so printing ten rows decodes ten blocks. `Interner::finish` encodes runs of blocks in parallel.
- **Cost:** `join_x` holds 32 bytes per row where it held 39, pinned by the budgets in `crates/table/tests/test_memory.rs`. Labels that share nothing cost their bytes plus about a byte and a half each.

## Equality joins bucket one side by dense codes of its own key range

**Status:** built.

- **The bucketed side is chosen from row counts before any key is coded** (`i` when it has at most 65,536 rows and no more than `x`, else `x`, the right table of `DataFrame.join`), and keys are coded against that side's range alone, so an out-of-range key of the other side gets a code no built row has. Coding over the union of ranges sent db-benchmark's q5 (range 2e8) to the Polars hash join, which did not fit in 15 GB.
- `indexable` admits a range up to eight times the rows below 2^27 codes and up to twice the rows above. `Buckets` is two flat arrays (`bounds`, `rows`) filled in parallel by code range; `scan_i` writes row pairs in parallel after a prefix sum, and `assemble` gathers one side, frees its row indices, then gathers the other. Every db-benchmark join at 1e8 rows runs in the kernel, and `tests/test_memory.rs` pins the transient memory (the bucket index and one side's row indices).

## A filtered `:=` reads a row index and writes in place

**Status:** built.

- **`assign` (`crates/frames/src/frame.rs`) writes in place when** the variable's own binding cell holds the frame (an active or locked binding never qualifies; storing the fork into a locked one raises R's error), the count is at most two (the finalizer holds one), and no outer `x[i, j, by]` of the same frame is running (`frame_queries`, because a nested update would change rows the outer query reads). Otherwise the update goes to a fork assigned back to the variable, through an active binding's function when there is one. Either way the update is stored before its warnings are raised, so a handler that unwinds sees the same frame whether or not it was shared.
- **A failed update changes nothing:** kernels and plans compute every column and check every removal before storing one; the R fallback (`fallback` in `frame.rs`) stores into a fork that replaces the table only after the last column. `assign_with_kernels` in `query.rs` checks types and shapes (`kernel::scatters`) before selecting rows, so an update the kernels cannot run builds no index and leaves no half-written column.
- **`Table::row_index` builds a column's `RowIndex` (the join's `Buckets`) on the second equality filter since the column last changed,** so a one-off update never pays for it: building reads the column twice and writes a row list as long as it, more than the one scan it replaces. It is built by one stable parallel counting sort, so each value's rows stay ascending. An Integer column qualifies when its value range is at most twice the rows plus 4,096, a Factor column when its level count is (a ten-row slice of a frame with three million levels would otherwise pay 12 MB of bucket bounds). `indexed_rows` recognizes `column == literal` in either order; every other filter scans. `scatter_values` writes a long ascending row list on the worker pool in disjoint stretches, and takes the serial, bounds-checked path when the last row lies outside the column.
- **The index is a secondary index, not a cache:** it answers another question (all rows of a value), as data.table's index built on the first `id4 == step` does. Its one invalidation point is `forget_column`, which every column change calls, plus `set_key`. Forks share it through an `Arc`. It costs 4 bytes per row plus 4 per distinct value until the column changes; `test_memory.rs` pins the loop at 5 bytes per row at peak and after (measured 4.1 and 4.0).
- **Measured:** db-benchmark's "`:=` on 1% of rows, 100 times" (4 cores, faster of two runs) takes 0.111 s from R and 0.105 s from Rust at 1e7 rows against data.table's 0.356 s, and 0.987 s and 1.040 s at 1e8 against 3.079 s; the run that builds the index takes 0.26 s and 3.0 s against data.table's first run at 0.40 s and 4.1 s.
- **Exact reference counts make in-place writes possible; each leak of a count forced a copy:**
  - a builtin S3 method releases the forced promises its dispatch made (`apply_method` in `dispatch.rs`, `Heap::release_argument`), because a collection that frees a promise does not lower its value's count;
  - a detached argument reference (`detach_reference` in `eval.rs`) gives up the pool's count (twenty `nrow(p)` calls on a classed list read a count of 20, and 100 update passes over 4e6 rows with an `nrow(x)` between them took 1.04 s against 0.077 s), and `pooled_reference` replaces a reference something else still counts, at one promise and a three-integer target, which is rare;
  - a released frame empties its `...` list (`Heap::release_dots`), stopping at a cell something else counts; a `...` list that outlives its frame (`get("...")`) is kept by `clear_frame`'s caller (oracle case `environments__dots_list_returned_from_its_frame`);
  - top-level printing rebinds its `x` to `NULL` afterwards (`release_printed` in `print.rs`), as GNU R's `PrintObjectS3` does;
  - a returning value is counted while exit code runs (`run_on_exit` in `eval.rs`), as GNU R's `endcontext` does, so `on.exit(x[1] <<- 0L); x` returns the body's value (oracle case `environments__on_exit_keeps_returned_value`).
- **An update nested in an update of the same frame is replaced by the outer one,** as `x <- update(x)` behaves when computing the update reassigns `x`.
- **The exact counts cost the interpreter 0.6% more instructions** per warm testthat repeat under callgrind (548.3M to 551.7M), with the same collections and resident memory: about 31,000 `...` lists emptied (`release_dots`) and 120,000 more values reaching zero at frame return (`note_zero`) over three repeats, plus one comparison per slot in `release_slots`.
- **Known limit:** a holder the counts do not see (native code through `R_PreserveObject` or the protect stack) does not stop an in-place write; no DataFrame builtin keeps a frame that way.

## A query without `by` aggregates in the kernel as one group

**Status:** built.

- `group.rs` runs `DataFrame.select(sum(...))` and `x[, sum(v)]` through the aggregate kernel as one group with no row codes (`whole`), so a sum reads the column in place. Polars materialized a cumulative sum (double) or a 64-bit cast (integer) of the whole column, 0.8 GB of scratch per column at 1e8 rows. The benchmark's checksum (`tools/bench/internal/table/ry.R`) asks the engine for the sums rather than converting an answer to R vectors.

## DataFrame expression operands are checked against the engine's own typing

**Status:** built.

- **`frame_expression` in `check.rs` checks operands with the rules of the engine's `Typer`** (`crates/table/src/query.rs`): arithmetic, unary `-` and numeric aggregations refuse text, `&`/`|`/`!` refuse text, and `Expr.if_else`'s test must be logical. The class comes from `join_class`, so an unknown or `Any` type stays silent. `ExpressionOperand` and `ExpressionComparison` name the column for a bare `Expr.col("x")`.
- **Deliberately stricter than the engine:** the engine compares an integer or logical with a string by formatting the number, as R does, but the checker reports text against integer or double because with a known schema that is almost always the wrong column. Text against logical stays allowed, so `Expr.col("x") == NA` is not flagged.

## Strict mode reports a DataFrame query over unknown columns

**Status:** built.

- **`note_unchecked_columns` in `check.rs` records a strict origin (`StrictOriginKind::UnknownColumns`)** on the subject of a verb that names a column (a string argument, or `Expr.col("x")` anywhere inside one) while the subject's columns are unknown (a computed `read_csv` path, a bare `DataFrame` parameter, an unreadable verb argument). A known `DataFrame` type with unknown columns is not `Unknown`, so without this strict mode passed every `Expr.col("x")` unchecked. A chain is reported once, at its first step that names a column; steps that name none are looked through. The message suggests the annotation with the first column named.
- **At the query, not at the read,** because the unknown schema has many origins and a frame nobody queries by column needs no schema; an annotation on the binding removes the origin.
- **`read_csv_type` matches `read_csv`'s own formals** (`path`, `strings_as_factors`), because borrowing `fread_type`'s argument whitelist made `strings_as_factors = TRUE` fall back to unknown columns. It shares `sniffed_columns` with `fread_type` and reading through `data_file_layout`, so a data-file edit re-checks only reads of that file (`a_data_file_edit_rechecks_only_the_reads_of_that_file`).

## CSV files are read in released slices straight into the final column buffers

**Status:** built.

- **`read_bytes` (`crates/table/src/io.rs`) parses about 256 MB slices** ending on a line break outside quotes (`slices`, which runs the `Quoting` state machine over each stretch in parallel from all three start states and chains them), appends each slice's columns to one `Builder` per column (the final buffer: a `Vec<i32>`, `Vec<f64>` or `Vec<u32>` of factor codes plus validity, reserved from the first slice's bytes per row), and `madvise(DONTNEED)`s the parsed slice. A factor column's slices intern into one `factor::Interner`. A whole-file parse plus a conversion copy held the file (5.2 GB at 1e8 group-by rows), the parsed frame and the R columns at once: a 9.9 GB load peak against 5.6 GB sliced.
- **A Polars parse error on a slice retries the whole file as text,** so a slice boundary never changes the answer. `check_csv_input` (the fuzz target) compares a 16-byte sliced read with a whole read.
- `aggregate` and `hashed` in `group.rs` drop each per-row and per-group buffer as soon as the next step no longer needs it, for queries with nearly one group per row (q10); the 1e8 group-by run then peaks at 13.4 GB from R and 12.6 GB from Rust against data.table's 13.7 GB.

## The Rust table benchmark runs on libR's allocator and the same lazy plans as the R path

**Status:** built.

- `crates/table/benches/allocator.rs` installs libR's allocator (`heap::large::Mapped` over mimalloc, eager commit off, purge delay 1e9 ms) as the global allocator, and the joins of `db_benchmark` run the same `Plan`s as `ry.R` rather than the `x[i]` IR, so "ry from Rust" and "ry from R" differ only by the API (on glibc malloc and the IR, Rust took 9.2 s at 1e7 against R's 6.3 s). A block of a megabyte or more gets its own mapping and returns to the kernel when freed, so peak memory is the engine's (12.0 GB from Rust against 11.6 GB from R on the 1e8 joins). The driver sums integer columns value by value, because a cast to doubles holds 0.8 GB beside a 1e8-row answer.

## Table operations map their large blocks on huge pages

**Status:** built.

- **A table operation holds a `heap::large::huge_pages` guard,** and while any guard lives `Mapped::alloc`/`realloc` ask for transparent huge pages on mappings of 2 MiB or more, because a query's buffers are fresh mappings and otherwise pay a page fault per 4 KiB (about 1.4 µs each on the measuring machine; group-by q1 at 1e7 took 0.106 s on fresh small pages against 0.024 s on reused ones in a non-LTO build). With the advice the 1e8 group-by takes 15.5 to 17.9 s from R (32.2 s before) against data.table's 59.3 to 65.3 s, at the same 8.5 GB peak. The advice is a no-op on a kernel without transparent huge pages. The guard count is one process-wide atomic, because buffers are mapped on worker threads. The interpreter takes it in every table builtin (`table_work!` in `crates/frames/src/frame.rs`) and in `frame::run`; benchmark drivers hold it for the run. Outside a guard nothing changes. `MappedPool` (large R vectors) never asks, because it resizes kept mappings.
- **Rejected: advising by size alone,** because the size cannot tell a buffer filled in full from a block reserved and filled in part (hash tables, growing vectors, the 16 MiB bytecode register file), and each would commit its last partial 2 MiB, raising every script's peak (by 2 MB for the register file, 0.86 MB for hash tables of 3.8 and 7.6 MB). A per-block opt-out (`MADV_NOHUGEPAGE` on the register file) fixed only the block that had one.
- **Rejected: only blocks of 32 MiB or more,** because the joins' 9 to 20 MB buffers went back to a fault per 4 KiB, costing the 1e7-row joins 15 to 40% (q3 0.14 s against 0.10 s).
- **Rejected for now: a pool of freed mappings in `Mapped`,** faster on reused pages (group-by q10 at 1e7: 0.43 s against 0.73 s on fresh huge pages) but keeps memory resident between queries where the 1e8 joins have about 3 GB to spare (`backlog.md`, "A query's large buffers are faulted in afresh each time"). The huge-page gain varies with how fast the kernel finds free 2 MiB blocks: the 1e8 joins took 8.1 s from R in one run and 11.1 s in another, and a slow run counted about 1,100 direct compaction stalls (`compact_stall` in `/proc/vmstat`); a pool would not depend on it.

## Group-bys and joins run on ry's kernels, not Polars' streaming engine

**Status:** built.

- **The kernels stay.** Polars' streaming engine (`db_benchmark --polars-streaming`, `crates/table/benches/streaming.rs`, feature `polars-streaming`) ran out of memory at 1e8 rows on exactly the two questions the 15 GB budget is about (group-by q10, nearly one group per row, and join q5), was slower on eight of the thirteen it finished (3.6 to 5.1 times on group-by q2, q8 and q9), and peaked higher at 1e7. Measured at 1e8 on 4 threads against Polars 0.55, faster of two back-to-back runs, factor columns as codes: group-by q2 0.18 s against 0.91 s, q8 2.08 against 7.82, q10 6.32 against out of memory, peak 8.5 GB against over 14.2 GB; join q5 5.60 s against out of memory, peak 11.9 GB against over 14.2 GB. At 1e7 streaming peaks at 2.10 GB against 1.15 GB (group-by) and 2.64 GB against 1.77 GB (joins).
- **What streaming does better,** kept as kernel work (`backlog.md`, "Open: DataFrame"): group-by q4 (three double means over 100 groups), where ry adds in row order as data.table does and Polars sums per morsel, and group-by q1 and join q1 and q3 by 10 to 35% (q4 0.22 s against 0.82 s). Group-by q5 is level.
- Polars' `streaming` feature is a dev-dependency of `crates/table` for the bench driver alone.

## The data.table migration is one engine in `ide`, read off the checker's types

**Status:** built.

- **`crates/ide/src/migrate.rs` (`data_table_rewrites`, `apply_data_table_rewrites`) is a pure read of the analysis,** used by code actions (each rewrite overlapping the viewport plus one whole-file action) and by `ry migrate data-table` (`cli::migrate_data_table`, analyzing as `ry check` does), so editor and CLI cannot disagree. It is not a lint fix, because a rewrite needs the target's type and columns.
- **A query `x[...]` is rewritten only when `x`'s type is a `Frame` or the `data.table` named type,** so a vector index is never touched.
- **With known columns a bare name is a column exactly when it is one of them,** as data.table looks columns up first. **With unknown columns, a bare name is a column unless it is a local (`item_naming().resolutions`), a project global (`project_global_names`), a base constant or a data.table symbol,** because a wrong guess fails loudly at run time with a missing-column error.
- **Nothing approximate:** unsupported shapes produce no rewrite. In-place updates (`:=`, `setorder`, `setDT`) are rewritten only at top level outside functions, as `x <- x |> ...`, because inside a function the caller would stop seeing the change. An existing-column `:=` is rewritten only for a literal of its own type, as `Expr.if_else(test & !Expr.is_null(test), v, Expr.col("col"))`, because data.table leaves a row whose test is `NA` unchanged. Keyed joins are skipped (they need statement order).
- **A rewritten table never escapes to unrewritten code,** because data.table's functions and `is.data.table` checks refuse a DataFrame. The whole-file migration keeps a rewrite only where its table flows into other kept rewrites, `$`, `[[`, a shape function, or is discarded; a function's returned query is wrapped in `data.table::as.data.table()`. Dropping one can expose another, so `data_table_rewrites` iterates to a fixpoint (`fate`, `uses_are_safe`). A single code action (`standalone`) is user-directed and stays offered. `tools/bench/tooling/migrate_fuzz.py` found the escape.
- **Accepted differences** (stated in `docs/src/content/docs/reference/from-data-table.md`): DataFrame aggregations skip `NA` (so `na.rm = TRUE` is dropped), `DataFrame.sort` puts `NA` last where `setorder` puts it first, and a join lists the left table's columns first.

## Dates, windows, ranks, cross joins and pivots in the table engine

**Status:** built.

- **`ColumnType::Date`** maps to Polars' `Date` and to an R double of class `"Date"` both ways (`table::convert::RVector::Date`, `libr::frame_altrep::date_vector`, `frames::frame::literal`); a fractional R date floors to its day. Date ± number is a date, date − date is `integer[]` days (deliberately not `difftime`: the engine has no duration column), dates compare only with dates, and any other operator is a `DateOperation` error (`date_operation` in `check.rs`).
- **`Ops.Expr` beats `Ops.Date`:** R refuses two incompatible `Ops` methods, so `chooseOpsMethod.Expr` returns `TRUE` (the R ≥ 4.3 protocol, honoured by `interp::dispatch`); the stub's `Ops.Expr` accepts `Date` on either side.
- **Windows and ranks lower to Polars** (`Expr::Window`, `Expr::Rank`: ordinal, ties by row, `NA` last); the kernels decline them. The checker types `over` as per-row with its argument's type and `rank` as `integer[]`. The ordinal row number is `is_null().cum_count()`, because Polars folds a constant `TRUE` mask into a length-one scalar.
- **Cross join is `How::Cross`,** keyless (`check_join_keys` rejects keys), left-major, clashing right columns suffixed; the checker's `join_type` treats it as a join with empty keys.
- **Pivot is eager** (`DataFrame.pivot` and `LazyFrame.pivot` collect, then run `table::ops::dcast`), because its output columns depend on the data, so no lazy plan knows its schema; the checker types it as an unrefined `DataFrame`. `dcast_dense` (`crates/table/src/ops.rs`) addresses cells as `row_id * labels + label_id` from order digits; the grouped path is the fallback when digits exceed a u64 or cells exceed `i32`.
- **Pushdown respects order and groups:** a filter reading a window, rank or aggregate moves only below a sort, and one that reads row order (`first`, `last`, `head`, `rank`: `lazy::reads_order`) not even there. Every pass needing an expression's columns uses `query::column_references`, which includes window keys.

## A `NULL` local inside a query of unknown columns is the column

**Status:** built.

- `time <- NULL` (R CMD check's "no visible binding" idiom) before `dt[...][order(-time)]` must read the column. `read_column` types a resolved `NULL` local as an `Unknown` masked column when the scope's columns are unknown, and on a plain `[` whose subject may be a data.table (declared, used with data.table syntax, or `Unknown`/`Any`/an open variable), `unseen_frames` makes `infer_read` turn a `NULL` local read into `Unknown`. A column scope was not pushed for the plain case because it masks every read, hiding a true unresolved name in `m[not_a_column]`.

## String keys sort by rank, and merge pairs come from sorted codes

**Status:** built.

- **A character key becomes `Column::Ranks`:** one `u32` per row, the rank among the vector's distinct strings, 0 for `NA` (`string_ranks` in `crates/interp/src/sort.rs`). Distinct strings are found with data.table's trick: each CHARSXP's truelength temporarily holds a negated slot index, checked against the slot list so a stale value is never trusted. Only distinct texts are sorted (collation for `order`/`sort`, bytes for radix), then ranks go through the integer radix sort. Copies of string data the heap already holds made peaks 1.17 to 1.37 times GNU R's; at 1e6 rows every base kernel's peak is within 1.13 times GNU R's (`order(chr)` 0.11 s against 1.9 s, `merge` 0.8 s against 2.2 s), and `make.unique` on 1e6 names takes 127 MB and 0.45 s (GNU R 92 MB, 1.1 s).
- **The truelength marks must be restored on every exit, errors included;** they survive allocation because the collector never reads truelength and the strings stay reachable.
- The radix sort scatters rows only, gathering `(key, row)` pairs only for buckets of at most 65,536 rows. `make.unique` (ASCII) marks names the same way; `paste(collapse =)` writes one byte buffer and one CHARSXP; `merge` finds matches by binary search in sorted `(code, row)` pairs, sized by a counting pass and written straight into the result.

## package:ry attaches no bare name another package exports

**Status:** built.

- **Every DataFrame function is qualified:** `DataFrame.set_key(df, cols)`, `DataFrame.key`, `DataFrame.copy`, `Expr.col`, `Expr.lit`. `DataFrame.set_key` is one function for both `setkey` forms, taking a character vector. The only bare names are the `DataFrame` constructor and the `as.*`/`is.*` conversions; everything else is under `DataFrame.`, `Expr.`, `GroupBy.` or `LazyFrame.`, and no name is exempt. `package:ry` sits at position 2, so any later package exporting the same name prints a mask message GNU R does not print (data.table's `setkey`, `key`, `copy` did), and bare calls get whichever was attached last; a bare `col` would mask base R's.
- **Enforced** by `no_shipped_package_exports_a_library_name` in `crates/interp/tests/test_standard_library.rs` against every `types/*.exports` manifest, failing when a package in `MUST_NOT_MASK` (base R, the 15 recommended packages, the benchmark dependency set) has none. Manifests are vendored so CI needs no GNU R: `scripts/export-manifests.R` records them, or with `EXPORT_SOURCES` from a NAMESPACE file when a package needs a system library or vendor SDK to install (Rglpk, Rblpapi).
