# Decisions: Analysis core

The analysis core's incremental structure, memory layout and cold-check cost: item identity, firewalls, tree storage, interning and per-item tables.

## One source of truth for where an item spells its name

**Status:** built.

`item_spans` classifies each top-level node through `top_level_definition`, which returns the item's
kind, name and the range spelling the name (the assignment target, or the `setGeneric` name inside its
quotes). `semantics::item_name_range(db, item)` re-reads that range off `item_node` at the rendering edge,
so item identity stays position-free; the IDE, duplicate-definition and unused checks all read it instead
of their own heuristics (which highlighted whole statements for `x <<- 1` or `setGeneric`). `:=` is not a
binding spelling: neither R nor the HIR (a call to `:=`) defines a name. A project-defined generic is one
global symbol, so its `setGeneric`, `setMethod` and `standardGeneric` strings and calls navigate,
reference and rename together.

## A check reads per-name firewalls, never project-wide maps

**Status:** built, except `interface_group` and the per-name lookups for names an item spells, which "Every host resolves names through one path, and trees live in the host's store" replaces with `file_interface_groups` and `item_links`. `package_name`, `test_name` and `frame_binder` serve the names a check constructs.

`item_check` never reads a project-wide map (`package_definitions`, every winner), because a new or
renamed definition anywhere would re-run every check. Each cross-item read goes through a small tracked
projection that backdates when its answer is unchanged: `package_name(files, name)` (winner plus
conditional writers) and `frame_binder(item, name, deferred)` (the file-local binder an immediate or
script read sees; a package function body never consults it). `SalsaGlobals` holds no map. `item_check`
grows its stack on demand through `stacker`, because demand recursion is as deep as the longest chain of
definitions that read the next. `crates/semantics/tests/test_incremental.rs` pins that a new definition
re-checks only its readers and an inserted statement only the reads it intercepts; the keystroke probe
cannot see this loss, because it edits one file and demands only that file. Cost: resident memory 3.5%
higher at 794K lines (1,270 to 1,314 bytes per line), cold time unchanged. Open gap (`backlog.md`): a salsa-level cycle through statement items
re-iterates on every structural edit.

## Cold-pass memory and time: interned names, compact HIR, parallel setup

**Status:** built, except the per-thread rowan token cache and the boxed-slice HIR lists, which "The syntax tree is ry's own flat preorder arena" and "HIR as struct-of-arrays, lowered from the green tree" supersede.

- `hir::Symbol` is a process-wide interned name, a `Copy` 4-byte index, carried by HIR, naming and the
  check output, because a name like `x` appears hundreds of thousands of times.
- `ItemCheck.expression_types` is a dense table indexed by `ExprId`, since ids are dense within an item.
- `ry check` forgets the database instead of dropping it (`std::mem::forget` in `crates/ry/src/cli.rs`):
  the drop only walks every allocation before exit.
- The binary uses mimalloc as a default feature; massif sees only the system allocator, so profile memory
  with `--no-default-features`.

## Syntax trees are evicted, and only small syntax facts are memoized

**Status:** built, except tree eviction and `RootDatabase::one_shot`, which "Every host resolves names through one path, and trees live in the host's store" supersedes. Kept for the rule that tree readers are memoized queries with small results.

- Every tree reader is a memoized query with a small result (`file_error_ranges`,
  `file_annotation_references`, `agreement_candidates`, `lints::file_syntax_lints`, `item_annotations`),
  so no consumer holds a tree for a few facts.
- A fact that also needs naming or project declarations splits into a naming-free memoized syntax half
  and a cheap join (`SyntaxLints.unless_redefined`, `SlotPair`'s `same_type` step), so the syntax half can
  be computed before naming.
- `diagnostics::derive_from_syntax` lists every tree-derived fact a later pass reads; a new tree reader
  must be added there, or the cold check re-parses (the one-parse test in `test_incremental.rs` catches it).
- Source text lives once, in `SourceFile`; the CLI reads it through `ProjectAnalysis::source`.
- The server enters workspace files at `Durability::MEDIUM` and lowers a file to `LOW` on its first edit
  (restored on close), so memos over untouched files verify without walking their dependencies; settled
  re-check time had been two thirds salsa memo verification.

## The cold check searches green trees and caches globals per item check

**Status:** built.

- `syntax::matching_nodes` filters with borrowed `NodeRef` cursors and makes owned nodes only for matches;
  `NodeRef::tokens` reads token text without owned cursors (`testing::check_parse_invariants` pins both to
  the owned walks). With the flat tree an owned cursor is only a refcount bump, so the gain is small.
  `map_child_types` returns the input type when no child changed.
- `SalsaGlobals` caches global lookups per `item_check` execution in `RefCell` maps. This is safe for salsa
  because the first read in a query records the edge and repeats only dedupe; the cache never outlives or
  leaves one execution.
- mimalloc's eager arena commit made the kernel zero transparent huge pages the process never used;
  `crates/ry/src/main.rs` turns it off from an `.init_array` constructor before the first allocation
  (peak RSS about 790 to 520 MiB).
- `infer::RESOLVE_CALLS` is thread-local: a shared atomic was a contended cache line.

## Cold-check constant costs: one green walk per file, immortal interning, high durability

**Status:** built, except the salsa-interned `Name` and `Ty`, which "Types and names are hash-consed outside salsa" supersedes, and `item_frame_binders`, which `item_links` replaces ("Every host resolves names through one path, and trees live in the host's store").

- `file_syntax_sites` collects every per-file site in one pass, and `lints::syntax_lints` scans in preorder
  with borrowed cursors, making an owned node only where `may_lint` says a lint can fire.
- The duplicate-type map is read once per file, not once per annotation.
- Interned names and types are never garbage-collected: free in a one-shot check, and in the server the
  tables grow only with distinct structural types and names, which repeat heavily.
- The CLI creates every input at `Durability::HIGH` except `PackageMetadata.attached`, the one input it
  sets after analysis starts; low durability made every syntax-pass memo deep-verify.

## Types and names are hash-consed outside salsa

**Status:** built, with the storage superseded: a type is an id into columns, not a pointer to an entry in a sharded table ("Types and symbols are four-byte ids into append-only columns"). Hash-consing outside salsa and the reasoning for never sweeping stand.

Types and names are hash-consed in a process-global table outside salsa. salsa interning (a shard lock and
an id-to-slot lookup per construction and per `kind` read; `Ty::intern_id` alone was 4% of samples) was
the largest constant cost of item checking and bought nothing: types carry no revision-dependent state.
Two databases in one process share the table, which is harmless because entries are immutable and
structural. A tracked function keyed by one name takes
`QueryName`, a salsa-interned wrapper, because salsa needs a salsa struct as a single key.

**Never swept**, because the type universe saturates under real editing: `TyKind` has no literal variant,
so changing a literal builds no type. Over 5,000 server body edits on the finance benchmark RSS levels
off at about 373 MB; only edits that keep inventing names grow memory, by about 240 B per new name (about
25 MB over 100,000 such edits). Add revision-scoped sweeping only if a workload
shows growth past that.

## A file's check names only what its interface reaches

**Status:** built, except the per-item `interface_group` walk and `item_interface_reads`: one walk per file finds the groups and `item_links` holds the edges ("Every host resolves names through one path, and trees live in the host's store").

Group membership is a property of what an item reaches, so it must not come from a whole-project graph;
otherwise the first check of one file names every item of the project (on dsbench, `item_naming` ran
56,546 times instead of 318, a one-file check took 2.3 s instead of about 0.25 s, a file opened at server
startup settled in 490 to 620 ms instead of 170 to 240 ms, and a keystroke after a cold prime took 240 to
290 ms instead of 60 to 74 ms). Every member of an item's strongly
connected component lies in its reachable set, so walking only what it reaches is exact, and members come
back in canonical order (`scc_exports` keyed by the first). `file_conditional_writers` screens items on the
file's tree (`binds_only_own_name`): only an assignment operator, `for`, `%<-%`/`%->%`, or a
`setnames`/`setcolorder` call outside a nested function can write a top-level name, so an item binding only
its own name needs no naming query. The screen may overcount, never undercount
(`testing::check_write_screen` asserts it in the fuzz battery). `file_global_variable_declarations` checks
the text for `globalVariables` before walking. The server's warm-up derives only per-file folds
(`warm_project_folds`) and names nothing. `file_conditional_writers` reads the file's tree, so it re-runs on
any edit to that file (one walk, backdating when the writers are unchanged).

## Every host keeps every tree

**Status:** built for the server. A command keeps a bounded number of trees in its `TreeStore` ("Every host resolves names through one path, and trees live in the host's store"). Kept for the measurement that salsa LRU capacities serialize parallel reads.

No salsa query has an LRU capacity. salsa records each read of an LRU query under one mutex per query, and
on four cores the cold check's reads of `item_syntax` and `parse` queued on those mutexes (the largest
single wait in a gdb sample); dropping only the item-level capacities removed the saving too, because item
subtrees are most of a file's tree. The cold check's three passes (derive from syntax, name, report) are
plain parallel loops (`in_parallel` in `crates/ry/src/cli.rs`), and tree readers stay memoized queries with
small results, so a check never walks another file's tree. Trade-off measured: cold `ry check` went from
about 2.2-2.5 s to 1.6-1.8 s while peak RSS rose from about 485 to 565 MB; mimalloc's remaining time is
fresh-page setup, and its purge and huge-page options made no difference. Remaining waits are salsa claims on
project-wide queries (the definition fold, `project_arithmetic_classes`, `project_type_definitions`)
that one thread computes while others need them.

## `ry check` releases its trees once, and memoized records are compact

**Status:** built, except the tree release (`release_trees`) and `item_interface_reads`, which "Every host resolves names through one path, and trees live in the host's store" supersedes.

- **Strictness has one decider.** `AnalysisSettings` carries `[check] strict` and `semantics::file_is_strict`
  decides a file's mode (the loader's `load_index::file_is_strict` reads the package default). The checker
  records strict origins only when strict, since non-strict projects never read them (about 14 MiB on
  the benchmark code bases).
- **Empty lists cost one word.** Most per-item lists are empty, and an empty `Vec` is 24 bytes, a boxed
  slice 16, which made `ItemNaming` 320 bytes and `ItemCheck` 248 across about 56,000 items. Frozen per-item tables are `SortedMap`/`SortedSet` (`crates/semantics/src/sorted.rs`) over a
  `ThinVec`, one pointer wide and allocation-free when empty; the same for check and naming lists and HIR
  side arrays.
- `ExpressionTypes` is a palette of the item's distinct types plus one `u16` slot per expression (`u32`
  past 65,535 distinct types).
- Interned types carry content flags (`HAS_VAR`, `HAS_RIGID`, `HAS_UNKNOWN`, `HAS_NAMED`) computed at intern
  time, so a walk skips a type with nothing to rewrite.
- `file_binders` is keyed by `Symbol`. Together these took peak RSS from 536-544 to 417-419 MiB.
  mimalloc's v2 build (489 MiB) and its purge, reclaim, retention and huge-page options, and a forced
  `mi_collect` at worker exit, measured no saving.

## HIR as struct-of-arrays, lowered from the green tree

**Status:** built, except `Module` owning one vector per array: it holds every array in a single word buffer.

`crates/semantics/src/hir.rs`:

- An `ExprId` indexes separate `kinds` and `ranges` arrays, because inference reads kinds and mostly only
  diagnostics read ranges; `Module::expressions` zips them for IDE scans needing both.
- A kind holds a `Span<T>` into one side array per list type, and literal text a `Text` span, so every kind
  is `Copy` (owned lists made readers clone to release a borrow). Children are visited with
  `Module::for_each_child` and `Module::any_child`, not a per-kind child vector. Literal text is not a `Symbol`, because the interner never frees and would keep every
  intermediate spelling typed in the editor.
- Lowering reads through borrowed `syntax::NodeRef` cursors, collects each list on a scratch stack, and
  runs in a per-thread scratch module that keeps its capacity; the memoized module gets exact-size copies
  ("scratch arena, owned compact result"). Red-cursor walking had been about 60% of `item_hir`'s samples.
- Names at their own site (argument names, fields after `$`/`@`, loop variables, tags) are a `NameId` into
  `Module::name_sites`, read through `Module::name`, `Module::name_range` and `Module::argument_name`. Ids
  are `NonZeroU32`, so an `Option<ExprId>` is four bytes; an expression kind is 16 bytes and an argument 8.
- `Symbol` hashes by a spelling hash computed at minting and orders by spelling, never by its index, which
  depends on which thread met a spelling first and would make output order vary between runs.
- `ExprId` allocation order is fixed, so ids, fixtures and downstream maps are stable. `Module` compares by
  value, so salsa backdating works. The checker records expression types in a vector indexed by `ExprId`
  (`RecordedTypes` in `check.rs`), not a hash map.

## A new top-level name costs one chunk fold

**Status:** built, except that `item_interface_edges` is part of `item_links`, which reads the project name maps once per item ("Every host resolves names through one path, and trees live in the host's store").

A name being typed is a new top-level definition on every keystroke, so it must fold only what it changes.
A layer's files split into interned `DefinitionChunk`s of 32 files; `definition_chunk` folds one into winners
and the names it repeats, and `definition_layer` holds the chunks plus a power-of-two bucket table (about two
buckets per name) of the last chunk defining a name in each bucket. `Definitions` answers `get` by probing
chunks from that bound downward: an undefined name, the common case for typo and unbound checks, costs one
array read. Duplicates are per file (`file_duplicate_definitions`, every definer of each of the file's own
names across chunks), which backdates while nothing duplicates. A single project fold (the previous shape)
cost 1.6 to 2.3 ms of the new-item edit on finbench, as one hash insert per name in 500 files; the layer
rebuild reads the same names but writes only a `u32` per name. Measured on finbench (4 cores): the new-item
edit went from a 2.9 to 3.1 ms median to 2.0 to 2.4 ms, body edits and the cold check unchanged. A
per-name `package_name` alone was rejected: the duplicate and typo checks need every name, and per-name
memos for names nothing binds cost about 100,000 memos. Map keys are `Symbol`, hashed by the spelling's FxHash
computed once at interning, not by index, because index order depends on which thread interned first.
`Symbol` therefore does not implement `Borrow<str>`: a lookup by text interns first (`Symbol::new`). The
symbol table stores each spelling's character count, which the typo-suggestion scan filters on before
reading any spelling. Pinned by `a_new_definition_reprojects_the_name_maps_and_checks_only_the_new_item`
and `a_new_name_refolds_only_its_own_chunk` in `test_incremental.rs`.
Measurement trap: a keystroke probe that edits a digit inside an identifier renames an item, and stubs
entered at `LOW` durability deep-verify every memo (about 51,000 per keystroke).

## Typed checking costs per name, not per file

**Status:** built.

- `TypeDefinitions` (`semantics.rs`) is a `Copy` view that looks a type name up in the file (scripts, or no
  `ProjectFiles`), then in the per-name projection `project_type_definition(files, name)`, then in the
  stubs; nothing merges the sets, because a merged map per file made typed analysis cost files times types
  (typed finbench: diagnostics 2.5 s against 0.2 s untyped; with the per-name view a typed check takes
  about 1.9 s / 644 MiB against 1.65 s / 533 MiB untyped).
  The unknown-type check builds the full known set only to rank suggestions after a miss. An edit to one
  type definition re-verifies only that name's readers.
- `file_comment_annotations` lowers each `#:` block once per file, keyed by start offset;
  `comment_annotation` finds one by binary search.
- `file_unresolved_non_locals` (`diagnostics.rs`) memoizes the non-local read classification per item, so
  the ordinary and strict checks share it.
- `ry load` reuses `ry check`'s record for its strict verdict: see "The strict load check shares the check
  record" (`libr-rcmd.md`).

## The cold check's per-call work: a bump arena, borrowed schemes, a token-only green builder

**Status:** built, except the token-sharing green builder, which "The syntax tree is ry's own flat preorder arena" supersedes.

- Argument matching allocates in a `bumpalo::Bump` per item check; the checker borrows `ItemCheck`s and stub
  schemes instead of cloning, and fetches the stub library once per check.
- Lookups take `hir::Symbol`, and `types::Name::new` accepts `IntoSymbol`, so no caller round-trips through
  a string. `hir::Symbol` orders by a cached 8-byte big-endian prefix before comparing text, so sorted
  symbol sets rarely touch the spelling.
- The lexer has ASCII fast paths; keyword lookup rejects by length and first byte.
- `ry check` reads and renders in parallel, prints in path order, and hashes sources only for the cache.
- `TypeDefinitions::of`, `read_context` and `SalsaGlobals::new` fetch stub library, project files and file
  kind up front: a few extra dependencies on rarely changing inputs, for one fetch per check instead of
  per lookup.
- The 1 s cold-check target is not met, and the remaining profile is flat (`backlog.md`, "Check cache
  follow-ups").

## The item check's working maps live in a pooled bump arena

**Status:** built, except the reads of a green tree, which "The syntax tree is ry's own flat preorder arena" supersedes.

- `ArenaMap`/`ArenaSet` (`check.rs`) are `hashbrown` maps in a `bumpalo::Bump` through an adapter for
  std's `Allocator`. Bumpalo's `allocator-api2` impl fails because the JIT enables hashbrown's `nightly`
  feature workspace-wide; bumpalo's `allocator_api` feature breaks Cranelift's register allocator.
- The per-thread arena pool is a stack, because an item check can start a nested check while its arena is
  live; arenas are reset, not freed.
- S3 method names are spelled in a per-thread buffer before interning (`method_symbol`).
  `substitute_rigid` takes a slice of bindings and memoizes only types larger than 64 nodes
  (`UNMEMOIZED_SUBSTITUTION_SIZE`), where sharing actually repeats work.
- Yes-or-no syntax scans build no owned nodes; the full argument list is built only when a name repeats.
- The pooled arenas cost about 24 MiB of committed memory on the finance bench. The remaining allocations
  are salsa's memo bookkeeping and scattered `Vec` growth (`backlog.md`, "Check cache follow-ups").

## One-shot checks record coarse dependencies

**Status:** superseded by "Every host resolves names through one path, and trees live in the host's store", which removes `Db::one_shot`. Kept for two measurements that still shape the code.

- A walk recording one salsa edge per project item leaves its handle's reused query-stack frame sized for
  the project, and every later query at that depth clears it (about 0.75 G instructions on the finance
  bench), so a project-wide name map is built on a throwaway database handle.
- The global allocator calls `mi_malloc` directly when alignment is at most 16 and no larger than the size;
  the `mimalloc` crate always took the aligned path.

## A cold check holds one parse tree per worker

**Status:** built, except `with_held_tree`, `parse_memo`, the per-item syntax memos and `Db::one_shot`, which "Every host resolves names through one path, and trees live in the host's store" supersedes: trees live in the host's `TreeStore`, and `RootDatabase::for_command(editor_facts)` replaces both one-shot constructors.

- `Db::editor_facts` off leaves editor-only facts (expression and pattern types, literal expectations, loop
  variables, selected overloads, callee targets) empty; it is its own switch because `ry migrate` reads
  expression types. `match_findings` reads the inference table, so exhaustiveness does not need them.
- `sourced_definition` returns early for a file that sources nothing (tracked part:
  `sourced_name_definition`). `TypeScheme::binders` is a `ThinVec` (one per item for the whole run).
- mimalloc's purge delay is 10 ms (`crates/ry/src/main.rs`), so trees freed mid-run return to the OS before
  the late peak.

## The one-shot check scans kind columns and overlaps its serial phases

**Status:** built, except the one-shot frame-binder path, `project_interface_groups` and the `SalsaGlobals::layer` memo, which "Every host resolves names through one path, and trees live in the host's store" supersedes.

- `NodeRef::descendants_of_kind`, `elements_of_kind` and `tokens_outside` scan the kind column and skip
  unwanted subtrees; lints, `write_tokens`, `collect_used_tokens` and `file_syntax_sites` use them.
- Item HIR is one exactly sized allocation; one `item_syntax_flags` query answers per-item syntax
  questions; rarely set `ItemNaming`/`ItemCheck` fields sit behind one optional box.
- The lexer uses `memchr2` for comment ends and string delimiters; `TreeBuilder::finish` is one backward loop.
- `ReadContext` fetches the file's package definition and conditional-slot maps once;
  `package_scheme_exists_in` takes a probe closure so both paths share one rule.
- `cold_check_memory_budget` (`crates/ry/tests/test_perf.rs`) fails a release cold check above 256 MiB.
- The profile is flat: the parser and tree builder (about 1.4 G instructions on finance), the lexer
  (about 0.65 G), spread salsa fetches and `infer_ordinary_call` remain.

## The checker's per-item facts are columns and bitsets indexed by dense id

**Status:** built.

Item-local ids are dense from zero, so hashing them wasted work on the hottest tables
(`Environment.entries` is probed on every read). `crates/semantics/src/dense.rs` has `Column<K, V>` and
`Bitset<K>` in the check arena, unallocated until the first write (most stay empty). Iteration is in id
order, so no result depends on hash order.

## Every host resolves names through one path, and trees live in the host's store

**Status:** built.

The server and `ry check` had separate implementations of name lookup, frame binders and cyclic groups, so
they could disagree, and trees had three homes.

- **`TreeStore` (`Db::trees`)** is the one home of trees; `parse` reads it and splices on changed text.
  Capacity is the host's only choice: every tree for the server, four per core for a command
  (`RootDatabase::for_command`). A command worker holds the file whose first pass it runs
  (`TreeStore::hold`); held trees are never dropped, the rest keep the most recently read. Holds alone were
  rejected: a read of an unheld file would reparse it every time.
- **`item_links(item)`** resolves every name the item's naming lists against the project name maps, and
  holds its interface edges and whether its file is in the project. It is the item's projection of the
  project-wide maps and backdates unless one of its names moved. It reads the project's file list but no
  fold over the project's names, so a name a test or script adds re-runs no package item's links.
- **Constructed names** (S3 methods like `+.Date`) go through `package_name`, `test_name` and
  `frame_binder`. A project package item skips `frame_binder` for a name the package layer does not bind,
  since every top-level name a package file binds is a definition or a listed conditional write; helper
  files' `test_name` layer works the same. Tests and scripts ask `frame_binder`.
- **`file_interface_groups(file)`** is one Tarjan walk from the file's items; groups list items, not
  positions, so they stay equal when a statement is added. `SalsaGlobals` keeps one column of resolved
  schemes by link position, because resolving copies a scheme.
- **Rejected:** a project-wide bound-name set (it lowered and named every file before the first check
  finished: one package file's cold diagnostics ran `file_binders` 1,006 times and `item_hir` 9,905
  times on the finance bench; and it is a fold over the project); a per-file name set (any new name
  re-checks the file); a `frame_binder` memo per constructed name (about 100,000 memos, 18 MB, nearly all
  for unbound names); edges in
  their own query (a second memo from the same inputs); one per-file links query (loses the per-item
  firewall).
- **Pinned:** `enable_invariants()` compares linked lookups with per-name answers, unbound verdicts with
  `frame_binder`, groups with a walk from the item. `test_incremental.rs` covers every way a package or
  helper file binds a name its check constructs (definition, conditional, chained, call-argument and
  in-loop writes, each failing if the verdict ignores the winner or conditional writers), query counts
  (a package file's check lowers no other file's item), and that a diagnostics-only host reports what an
  editor reports.

## Types and symbols are four-byte ids into append-only columns

**Status:** built, except the per-item store for types that contain inference variables, which
is rejected below.

Interner lookups took a shard mutex that checking threads contended for, hidden behind four caches.

- `crates/semantics/src/intern.rs`: `Column<T, CHUNKS>` is append-only in fixed chunks of 4,096 that
  never move, so a read is a shift, a load and an offset;
  `IdTable<W>` is an open-addressing hash-to-id table whose slots fill once and that grows by replacement,
  so lookup is lock-free and only storing takes a lock.
- A `Ty` is a `NonZeroU32`: low 27 bits index the kind and facts columns, upper bits say whether it is or
  contains a variable, named type, rigid parameter or `Unknown`. `TyKind` is `Copy`, its child slices in a
  bump arena behind the writer lock. The 15 leaf kinds and the first 4,081 inference variables fill the
  first chunk as constants. A `Symbol` is an id into a column of `{ text, hash, first eight bytes, length }`,
  hashes as its stored spelling hash and orders by spelling; `Symbol::lookup` probes without interning.
- `resolve` and `substitute_rigid` return on the id bits when nothing can change, and memoize subtrees for
  one call only for large work (`resolve` after 64 steps, `substitute_rigid` above 64 nodes).
  `InferenceTable::named_types` is two inline lists searched front to back, not a hash map: 91% of 1.08
  million definition lookups on the finance bench repeat a name the item already asked about, and an
  uncached answer is two salsa fetches and a scan.
- **Rejected:** a per-item store for types with variables (they leave the item via `EditorFacts`,
  `TypeErrorKind` and diagnostics, so every exit would copy, and only 696 of the 2,589 distinct types a
  finance check builds contain one). Never freed (see "Types and names are
  hash-consed outside salsa"); a full store (2^24 types, 2^28 spellings) panics.
- **Invariant:** id order differs between parallel runs, so nothing reaching output may walk ids in order;
  that is why `Symbol` orders by spelling.
- A finance check executes 18.06 G instructions against 19.07 G with pointer types.

## An item's syntax has a stamp, so an edit lowers only the items it changed

**Status:** built.

Reading an item's range from `item_spans` made every edit re-lower every item of the file.

- The store keeps a `SyntaxStamp` per top-level node. On reparse, nodes at the file's start and end whose
  subtrees equal the old ones (an exact comparison) keep their stamps, others get fresh ones from a
  store-wide counter; equal stamps imply equal subtrees.
- `item_stamp(item)` backdates outside the edited region; `item_syntax` reads only the stamp and finds the
  node through a per-file index, falling back to a fresh parse when a bounded store dropped the tree.
- A `#:` block is a top-level node, so it has a stamp too. `item_spans` records the stamp of the block
  attached to each item, and `item_annotation_syntax` depends on `item_annotation_stamp` alone, so an edit
  to one block lowers only its item's annotations. This replaced a per-file memo of detached annotation
  subtrees, which every item's annotations read, so an edit to any block re-ran all of them and the memo
  kept syntax alive outside the store.
- `item_name_range` reads `item_spans`.
- `TreeStore` has 16 shards; a parse runs outside every lock, and a second reader waits for it.
- **Rejected:** the stamp in `item_syntax_flags` (other items read those flags, so they would re-run on a
  body edit); a content hash (a collision keeps a stale lowering unnoticed); matching moved nodes anywhere
  (one edit changes one region, so the linear prefix and suffix comparison suffices).
- Known cost: edits at both ends of a file between two reads make one region spanning the file, so the
  items between re-lower and `item_hir` backdates. The stamp queries take 3.3 MB at the finance heap peak
  (50,815 items).
- **Pinned:** `enable_invariants()` checks each stamp read, the annotation stamp's too; `test_incremental.rs`
  pins query counts for an edit inside an item, a new item above the others, a removed item, the second of
  two equal statements removed, edits at both ends, and an edit inside one `#:` block.

## The analysis core's per-item and per-file tables after the type store

**Status:** built.

- `item_links` has one memo per item for the whole run (50,815 on the finance bench), so `ItemLinks` is two
  thin vectors (`words`: header, names, one word per binding holding its name's position above its kind;
  `items`: binding items, then edges), two pointers wide, and allocates nothing when empty. Its memos hold
  10.5 MB at the finance heap peak, and it is 4.6% of a check's instructions, mostly resolving every listed
  name (`backlog.md` has it). Edges sort by file, spelling, kind and disambiguator, not `Symbol::index()`, which is not
  reproducible.
- `file_follows_strict` is the one answer to "is this file strict"; `item_tree_positions` the one
  item-to-position map; only an item binding more than its own name keeps `item_bound_names`. `Item.name`
  is a `Symbol`, and the stub library's collections are `SymbolMap`/`SymbolSet`.
- The server derives the attached set from `file_attached_namespaces` scans rather than a hand-kept copy.
  `ide::spelled_names` lets tag completion and searches open only matching files.
- The server worker's token flips only inside an armed scope (`Worker::armed`, which `Worker::cancellable`
  and request reads use), so a read outside one cannot unwind the worker; see "A cancellation flips the
  worker's token only inside an armed scope".

## `item_links` resolves every name an item spells

**Status:** settled (lazy resolution not taken).

- The cyclic-group walk (`item_interface_edges`) needs every bound edge before any check runs, and only resolving a name says whether it is bound, so a lazy `item_links` would still resolve every name before checking. It would defer the work, not skip it.
- Measured on the finance bench (1006 files, 50,815 items): `item_links` resolves 303,616 (item, name) pairs, 77,186 of them bound, and checks ask `SalsaGlobals::bindings` for 201,974 distinct pairs. The most laziness could skip is the third the checks never ask about, which the edge walk asks about anyway.
- Fewer memos or fewer recorded edges would shrink its 10.5 MB, not a different resolution order.
