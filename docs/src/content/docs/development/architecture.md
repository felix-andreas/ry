---
title: Architecture
description: How ry builds its analysis stack, from the hand-written parser to the salsa semantics database and the language server
---

The crate boundaries, the analysis database, and the language server's scheduling described here
are the contract for the implementation. What the type checker *means* is specified separately, by
the [typing reference](/reference/type-system).

## Crate graph

Layering is enforced by the crate graph. A crate boundary is the compiler-checked way of making an
illegal dependency unrepresentable.

- **`syntax`** is a hand-written lexer and a recursive-descent parser with Pratt parsing for
  expressions, emitting a flat syntax tree (see [The syntax tree](#the-syntax-tree)). Three
  properties matter downstream:
  - The trees are *lossless*: every byte, trivia included, reprints exactly.
  - Parsing is *error-resilient*: every parse yields a tree, and an error stays local to the place
    where the code broke.
  - Items are *position-independent*: an item's syntax is a window onto the file tree with offsets
    counted from the item's start, so an untouched statement's tree stays equal when something
    elsewhere in the file changes.

  A `#:` annotation is lexed as structured trivia and parsed into first-class nodes with real spans,
  so type syntax is grammar rather than comment text scraped after the fact. Inline annotations run
  the same type grammar on R's token stream, in `TYPE_ASCRIPTION`, `RETURN_TYPE`, and a function's
  `TYPE_BINDER_LIST` nodes, and `erase_inline_annotations` gives the plain R program an annotated one
  means. The crate depends on nothing semantic.
- **`semantics`** holds the salsa database and every analysis query: the per-item item tree and HIR,
  naming, interned types, Hindley-Milner inference, the stub corpus, the lints, and the point where
  diagnostics leave the analysis. It depends on `syntax` and salsa.
- **`ide`** implements the editor features (hover, navigation, rename, call hierarchy, completion,
  inlay hints, signature help, code actions and fixes, symbols and the outline, folding, selection
  ranges, and semantic tokens) as pure reads of `semantics` queries at byte offsets.
  Anything to do with the editor protocol lives in the server, never here. Reading an installed
  package's R code is I/O, so the server passes it in as a callback (`completion_with`,
  `signature_help_with`) rather than the analysis reading the R library itself. Completion ranks
  names by how often the project uses them, summed over tracked counts per chunk of 32 files, so a
  keystroke recounts one chunk instead of the whole project.
- **`format`** is the formatter. It depends on `syntax` alone, and because the compiler enforces
  that, it can never come to depend on an analysis result.
- **`loader`** is the package-development loader. It reads a package into the analysis, diffs the
  load index against what a session has loaded, and produces a backend-neutral plan. Its GNU R
  backend hands the plan to R code as an RDS file, and its backend for ry's own runtime (behind the
  `runtime` feature, which links `heap` and `interp`) builds the plan on the heap and binds each lazy
  definition as a promise over the analysis's own syntax tree.
- **`oracle`** is the differential test harness for the R runtime: it runs one R file under GNU R
  and under another engine and compares the observable results. It also generates the programs for
  differential fuzzing and runs the corpus packages' own tests and installs. It depends only on
  `syntax`.
- **`repl`** is the R console behind `ry run` and `ry R`. It loads R at run time, so no crate,
  `repl` included, needs R to build. For `ry run` it loads ry's `libR.so` instead of GNU R's,
  and `ry` itself does not link `interp` or `libr`.
- **`heap`** is the R runtime's object model and garbage collector: the `SEXP` object layout,
  allocation in bitmap-managed pages, the non-moving generational collector and its roots,
  `CHARSXP` interning, symbols, environment frames and the `R_alloc` arena. It depends on nothing else in the workspace.
- **`libr`** is R's C API, built as the shared library `libR.so` that compiled packages link
  against, with the C half of the native boundary and an R 4.6-compatible header set. It depends on
  `heap`, `interp`, `jit`, `table` and `loader`. Its design is the draft at
  [`development/design/r-runtime.md`](/development/design/r-runtime/). Evaluation belongs to the
  interpreter, which plugs into it through the `Evaluator` hook in `eval.rs`.
- **`interp`** is the R interpreter: the evaluator, the tier-0 bytecode, conditions, S3 and S4
  dispatch, and base R's primitives and internals in Rust, plus the check of
  [inline annotations](/tutorial/runtime#inline-annotations) at function boundaries. It depends on
  `heap`, `syntax` and `table`, and `libr` installs it through the `Evaluator` hook, so it knows
  nothing about C. The values that are ry's own live here too: records and tags, `List`,
  `DataFrame` and the standard library.
- **`jit`** is the runtime's optimizing tier: it compiles hot tier-0 bytecode to machine code with
  Cranelift. Tier 0 calls it through the `Tier` hooks in `interp`'s `bytecode.rs` when a body has
  been entered often or a loop has jumped back often, and the compiled code runs on tier 0's own
  register window, so a failed speculation hands the activation back to the interpreter mid-body.
  It depends on `heap` and `interp`, and `libr` installs it after the interpreter. `RY_JIT` picks
  the mode: `on` (the default), `off`, or `force`, which the tier tests and the `ry-jit` oracle
  engine use.
- **`typed`** is the typed core. A strict typed closure called with values whose types it accepts
  is lowered from its body to a typed IR at its first call, given reference counts, and compiled
  with Cranelift to run on native values: scalars in registers, 16-byte strings, and lists that
  view an R vector's data or own their items. `jit`'s `native::call` is the only caller, and a
  call that does not compile, or fails while it runs, goes back to the interpreter unchanged. It
  depends on `heap` and `interp`. `RY_TYPED_LOG=1` prints each compilation and refusal, and
  `RY_TYPED_DUMP=1` prints the Cranelift IR.
- **`table`** is ry's built-in typed table: an engine over Polars with a query IR that lowers
  `tbl[i, j, by]` and the `DataFrame` verbs to a lazy plan, plus the readers, writers and
  data.table helpers around it. `interp` owns the `DataFrame` value and calls the engine, and `libr`
  exposes a column to native code as an ALTREP vector, so the table holds only Arrow data.
- **`sniff`** reads the layout of a delimited text file: separator, header and each column's R type.
  The table engine's reader and the checker both use it, so the checker types a literal
  `fread("prices.csv")` as exactly the columns the engine reads. It depends on `syntax` alone.
- **`rcmd`** is ry's R home and the `R CMD` commands that build and install packages against
  ry's headers and `libR`. It depends on nothing else in the workspace; `libr`'s tests use it.
- **`ry`** is the product: the LSP server and the CLI, with the `check`, `fmt`, `server`, `run`,
  `R`, `load`, and `debug` commands. It owns configuration, conversion between position encodings,
  assembling and publishing diagnostics, and suppression comments.

## The semantics database

Analysis runs on salsa and is incremental at the granularity of an *item*: one top-level statement
of a file, such as a function definition, a value binding, or a bare call.

### Inputs

The analysis of a project has five inputs:

- `SourceFile` carries one file's text and document kind.
- `ProjectFiles` is a singleton listing the project's files: package documents first, in the
  `DESCRIPTION` `Collate` order when one is declared and by path otherwise, then the rest by path.
  The last-writer-wins symbol index and the CLI both rely on that order.
- `PackageMetadata` is a singleton carrying the imports from `NAMESPACE` and the dependency universe
  from `DESCRIPTION`.
- `StubSources` is a set-once singleton carrying the `.Rtypes` corpus.
- `AnalysisSettings` is a singleton carrying the configuration that changes what analysis means
  rather than which findings a host publishes: `[check] inline-annotations` and `[check] strict`.
  The first is read only where an inline annotation exists, so toggling it re-checks only the
  items that have one. The second is the project default behind `semantics::file_is_strict`,
  which decides a file's mode for the check (its `# typing:` directive wins). The package loader
  keeps its own `load_index::file_is_strict`, which reads the loaded package's default instead. A check records the
  origins strict mode reports only for an item in a strict file, so a project that is not strict
  never builds that table.

The package loader adds a sixth, `PackageSources`, described [below](#the-package-load-index).

### The syntax tree

A file's tree is one `syntax::Tree`: its text and four parallel arrays over its elements (nodes and
tokens alike) in preorder, holding each element's kind, start offset, the index one past its
subtree, and its parent's index. Kinds are numbered tokens first, so a comparison tells a node from
a token, and an element's end is where the next element after its subtree starts. A subtree is
therefore a contiguous index range and a node's text a contiguous slice of the file text, so a
whole-tree walk is a linear scan and reading a node's text never allocates.

An item's tree is a window onto its file's tree, not a copy. A `Tree` holds the file's arrays
behind an `Arc` plus the window's first index, length and base offset, and every accessor
subtracts those, so an item tree reports indices from zero and offsets relative to the item.
Equality compares kinds, subtree ends and those relative offsets, which is why moving an item
within its file does not invalidate what was computed from it. Copying item ranges instead would
duplicate every file's arrays, about 114 MiB on the finance bench.

Cursors are indices. An owned `SyntaxNode` or `SyntaxToken` is the tree's `Arc` plus a `u32`, so
moving one is arithmetic and a clone is one reference-count increment. A borrowed `NodeRef` is the
tree reference plus the index and costs nothing to copy, which is what the whole-file passes and
HIR lowering use. Lookups that a nested tree answers child by child use the layout instead: starts
never decrease in preorder, so `node_at`, `token_at_offset` and `child_or_token_at_range` binary
search the starts, and a previous sibling is found by climbing parents from the element before.

The parser emits elements in postorder, because it only knows that a node wraps the elements after
a checkpoint when it opens the node there. `TreeBuilder::finish` lays them out in preorder in one
backward pass: an element's preorder index is its parent's plus one plus the size of its earlier
siblings' subtrees, which postorder gives as a difference of first-element indices. The parse
invariants (`syntax::testing::check_parse_invariants`) check every shortcut against the
child-by-child definition on every parsed program, and with `enable_invariants()` every splice
reparse is compared with a parse from scratch.

### Parsing, and why it is not where incrementality happens

`parse(file)` is a per-file query with a splice cache beside it. The cache remembers the file's
previous text and tree (shared across clones of the storage handle), works out the current edit as
the longest common prefix and suffix, and lets `syntax::reparse` reparse only the statements the
edit touched and copying the untouched top-level runs of the old tree around them.

The spliced tree is byte-identical and error-identical to a parse from scratch. The edit-stream
fuzzer pins that equivalence, and the splice refuses and falls back to a full parse whenever the
equivalence is not provable, so the cache is invisible to every query downstream. Invalidation below
the level of a whole file stays out of salsa on purpose.

The real incrementality happens one level down, in the item tree. An item's identity interns its
kind, its name, its parent, and a disambiguator, which makes it stable when other items are
inserted around it. An edit inside one item therefore leaves the derived values of its siblings
equal, and salsa's early cutoff prunes all the work downstream of them.

### Syntax trees stay resident in the server, and `ry check` holds one at a time

The server keeps every syntax tree, because an edit reparses one file and every other tree stays
valid. `ry check` reads each tree in its first pass only, so it does not memoize them. Each worker of
the syntax pass wraps a file's work in `semantics::with_held_tree`, which parses the file once and
parks the tree in a thread-local slot that `parse` serves for the rest of that file's work; the
tree is freed when the worker moves on. In a one-shot database `item_syntax` and
`item_annotation_syntax` skip their per-item memos as well, because a memo would keep its file's
tree alive past the syntax pass and buys nothing in a run that never edits. A tree read outside a held
tree still works: it goes through the `parse_memo` query, which then keeps that file's tree for the
rest of the run. A full cold check never does this, but a partial cached run, the cache planner's
surface comparison and `--annotate` read some trees that way. Annotation memos (`file_item_annotations`) store
detached copies of their subtrees (`syntax::SyntaxNode::detached_subtree`), so on any host an
annotation never keeps the whole file's tree alive.

Every query that reads a tree is a memoized query whose result is small: the item HIR and
annotations, `parse_stage_diagnostics`, `file_error_ranges`, the annotation references and
agreement candidates in `diagnostics.rs`, `item_name_range`, `item_syntax_flags` (whether
an item binds only its own name, and whether it is declared pure), and `lints::file_syntax_lints`. So a check never walks another file's tree, and revalidating an
untouched file after an edit elsewhere never walks its own. A finding that also needs naming or
the project's declarations is split in two: the syntax-only half is memoized, and a cheap step
joins it with the rest (a bare `setClass` call is reported only when the file does not bind
`setClass` itself, and two annotations disagree only when their types differ once aliases
expand).

The CLI's cold check runs three passes over the files, largest first, spread over the cores.
The first computes every tree-derived fact (`diagnostics::derive_from_syntax`), including each
file's share of the project-wide maps (`warm_project_folds`), so that no later check derives
another file's facts on its own thread. The second names every item, so that two threads
checking files that reach the same item do not block on one claim, and the third assembles the
diagnostics. `RootDatabase::one_shot` turns off the splice cache, which would otherwise keep the
previous tree of every file. `test_incremental.rs` pins that a cold check parses each file once
and that a one-shot cold check memoizes no tree.

Most tree readers look for a handful of node kinds in a large file, so they walk borrowed cursors
(`syntax::NodeRef`), which touch no reference count: `syntax::matching_nodes` takes a predicate
over a node and its parent and makes owned nodes only for the matches. The per-file site collectors
share one pass over the nodes, `file_syntax_sites`, which tests each role only on the kinds that
can carry it. The syntax lints scan the kind column for the element kinds a lint or an annotation region
can start at (`NodeRef::elements_of_kind`),
track the enclosing annotation region by position (`NodeRef::encloses`), and make an owned node
(`syntax::node_at`) only where a cheap test says a lint may fire. A borrowed `TokenRef` reads its
kind, range and text only when asked, because a child walk mostly checks kinds.

The cold check is CPU-bound and scales about 3 times on 4 cores, so per-file constant costs set its
wall time. Several of them are worth knowing:

- Every salsa read of a tracked function with several arguments interns its key and verifies a
  memo. So in the server one item check resolves all its frame binders in one query
  (`item_frame_binders`), and every item check caches the package names and schemes it looks up
  in `SalsaGlobals`. `package_name` and
  `test_name` take only the name and read `ProjectFiles` themselves.
- `Ty` and `Name` are not salsa structs. A `Ty` is a pointer to a hash-consed entry in a global,
  64-way sharded table (`crates/semantics/src/types.rs`), so equality and hashing are a pointer
  compare and reading the kind is a field load. A direct-mapped per-thread cache sits in front of
  the shards, because checkers intern the same few types over and over and those land on the same
  shard mutexes. The entry stores the type's size, which the
  inference-size cap reads without a walk. A `Name` is a `Symbol`. Neither is ever freed: a
  one-shot check loses nothing, and the server's table grows only with distinct types, which repeat
  heavily because types are structural. A query keyed by one name takes a `QueryName`, the one
  salsa-interned wrapper left. `Symbol::new` checks a thread-local table before the shared one.
- The checker matches call arguments in a `bumpalo` arena that lives for one item check, and it
  borrows the item's checks and the stub schemes instead of cloning them.
- A `NAMESPACE` over 64 KiB parses in chunks on all cores (`metadata::parse_namespace`). Chunks split
  only before a line that opens a call and follows a line that is exactly one complete top-level
  call, so no directive straddles a boundary.
- The CLI creates every input at high durability except the attached namespaces, which it sets
  after the first pass. Memos that never read that set skip verification in the new revision.
- Every item keeps its HIR, naming and check memos for the whole run, so each 8-byte field on
  those values costs about half a megabyte across a 300K-line bench. `ItemNaming::extras` and
  `ItemCheck::editor` keep the rarely populated and the editor-only fields behind one optional
  box, and a one-shot check skips the per-item binder memo (`item_frame_binders`) and reads the
  file's binders, a sorted array searched by name, directly.
- mimalloc's `realloc` keeps the old block when the new size is at least half of it, so a `Vec`
  shrunk with `into_boxed_slice` or `shrink_to_fit` can still hold twice its length. A buffer that
  stays memoized is allocated at its exact size up front instead.
- mimalloc commits its arenas lazily (`crates/ry/src/main.rs`): eager commit made the kernel zero
  huge pages that a short-lived process never touched. What allocation still costs is mostly
  setting up fresh pages, which scales with the memory the analysis keeps, and mimalloc's purge
  and huge-page options measured no difference.

### The check cache

The cold check already uses about 3.4 of 4 cores in every pass, so more parallelism cannot bring
it much lower. What a repeat run can skip is work whose result is already known, so `ry check`
records each file's findings in `.ry_cache/check.json` (`crates/ry/src/check_cache.rs`). The salsa
memos themselves are not persisted. Salsa 0.28 exposes no dependency graph to save, and its
persistence feature would need every memo to serialize, including the process-wide `Ty` table.

The record has a key fingerprint and, for each file, four facts:

- **Key**: the ry version and build, the configuration, the project root, the data files,
  `NAMESPACE`, `DESCRIPTION`, and the project's stubs. A different key checks every file.
- **Content fingerprint**: the file's text.
- **Surface fingerprint**: two parts. The first is the text with the body of each top-level
  function definition left out, unless the body contains `<<-`, `->>`, or a call that declares or
  attaches something (`assign`, `library`, `setClass` and the rest of `DECLARING_CALLS`). The
  second is the file's share of every project-wide fold, listed by
  `diagnostics::project_fold_shares`: its definitions, conditional writers, type declarations,
  `globalVariables` names, S3 generics and attached packages. A body can feed a fold, as a
  `UseMethod` call or a `#: @type` inside it does, so the shares come from the queries rather
  than from the text. A new fold over the project's files must add its share there.
- **Dependencies**: the files whose items the check read through interface edges, transitively
  (`semantics::file_dependency_files`), plus the files that its findings point into.
- **Imports used**: the `NAMESPACE` imports the file's code uses, which the `NAMESPACE` findings
  depend on.

A run with no changed file replays every finding without building a tree. A run whose changed
files all keep their surface checks the changed files and the files whose dependencies include
one of them. Every surface then matches the record, so the attached set comes from the record as
well, and the `NAMESPACE` findings are recomputed from the imports used, which costs a few
milliseconds. Any other change checks every file.

A package-only run, such as `ry check R` or the strict check of `ry load`, analyses only the
package files. Tests and scripts read the package namespace but add nothing to it, so leaving
them unparsed changes no package finding. They stay in the record: a file the run left unanalysed
keeps its recorded content, surface, findings and imports used while its text is unchanged. When
its text did change, the record keeps no surface for it and marks every unanalysed file
unchecked, so the next run that analyses them checks them all. The `NAMESPACE` findings read only
the imports the analysed files use, so a package-only run and a full run can disagree on them
(an import only the tests use is unused to the first and used to the second). A replay therefore
recomputes them when the package files' imports differ from all files' imports. On the benches, a file's dependency closure has a median size of 0 and a maximum of 1, so
an edit inside a body rechecks one or two files.

A partial run still parses every file, because the project-wide maps fold every file's items. So
it warms those folds across all cores, and that parse, about 0.3 s of the 0.45 s total, is its
floor. Removing that floor would mean persisting the item trees. When invariants are on
(`syntax::testing::enable_invariants`), every run that reuses findings is compared with a fresh
run and panics on any difference. The seeded edit simulation in `cli.rs`
(`cached_checks_match_fresh_checks_across_seeded_edits`) drives that comparison.

### From item to scheme

An item's subtree lowers to HIR, with spans relative to the item. Its annotations lower
separately: a `#:` block and an inline annotation go through the same type lowering into the same
`Annotation`, and a statement annotated both ways gets one merged annotation
(`annotations::merge_annotations`), so the checker never knows which spelling it was given. Names
in HIR and naming are interned `Symbol`s (a 4-byte `Copy` id). Literal text is not interned,
because the interner never frees and an edit inside a string literal would otherwise keep every
spelling it passed through; it lives in the module's own text buffer, addressed by a `Copy`
`Text` span. The HIR module is
struct-of-arrays: an `ExprId` indexes separate kind and range arrays, and every list (arguments,
parameters, statements, record fields, match arms, pattern lists) lives in one side array per
element type that the expression addresses with a `Span`, so every HIR kind is `Copy` and a scan
over kinds never touches ranges. A name written at a site with its own range (an argument name, a
field after `$` or `@`, a loop variable, a tag) is a `NameId` into a `NameSite` side array that
holds the symbol and its range together, and every id is a `NonZeroU32`, so an optional child costs
no extra word. An expression kind is 16 bytes and an argument 8. Lowering reads the tree through
borrowed `syntax::NodeRef` cursors into a per-thread scratch (`hir::Parts`) that keeps its capacity
across items. Sealing copies every array and the text into one allocation of exactly the needed
size, a word header of section starts followed by the sections, so the memoized `Module` costs one
allocation and no spare capacity. When every range in the item ends below 64 KiB, as almost all
do, a range packs into one word. Naming
then runs the variable model, in which a variable is a mutable slot with reaching-write flow.
Finally, the inference walk, `item_check`, produces the expression types, the type errors, the
strict-mode origins, and the scheme the item exports.

**An inference variable must never leave the item that created it.** A variable is an index into one
item's inference table, so nothing that outlives that table may hold one. An exported scheme is
closed at the item boundary by `close_scheme`, and a value cached across a rollback of the table has
its variables erased first. A leaked id is not merely a crash risk: in a reader's table it either
points past the end, which panics, or it silently names some unrelated variable of the reader's own,
which is worse.

To keep that from happening by accident, every transformation that rebuilds a type (resolution,
substitution, generalization, erasure) goes through one structural map, `map_child_types`. That way
no walk can handle a member, such as a parameter's default, that another walk forgets. Reads go
through the matching `for_each_child_type` and `Parameter::types`.

### Cyclic definitions

When definitions refer to each other in a cycle, the package interface resolves the whole group
through one canonical fixpoint, so the result never depends on which member happened to be queried
first:

1. `interface_group` finds the cyclic group an item belongs to. `item_interface_edges` gives each
   item, definition or top-level statement alike, the items its reads resolve to, following
   exactly the resolution a check performs: the frame binder in the item's own file, then the
   test and package layers, and every conditional writer of a name that has no unconditional
   winner. An iterative Tarjan walk from the item visits only what it reaches, which is enough,
   because a cycle through the item lies inside that reachable set. Members come back in
   canonical project order, and the first one keys the group.
2. `scc_exports` starts every member at `Unknown` and runs Jacobi rounds until the table of
   exports (each member's scheme and the schemes of the top-level names it binds) stops changing.
   If the round cap is reached, every member is pinned.
3. `item_check` adopts the canonical exports as the one exported truth.

A member check inside the fixpoint never re-enters `item_check`, so no salsa cycle forms. Salsa's own
cycle recovery remains only as a backstop, for a reference the static graph cannot see.

The graph must cover every edge a check can follow, not just the common ones. Salsa never backdates
the memo of a query that took part in a cycle, so when an edit breaks a cycle the former members
re-run with no cycle heads and salsa's backdating check panics on any member whose value did not
change. A missing edge turns into exactly that panic on the edit that removes and restores it.

That backstop has a trap of its own: **a cycle recovery must not pin a value that depends on the
round that produced it.** Salsa stops iterating when a recovery returns the same value as last time,
so the value pinned at the round cap has to come back unchanged. A pin computed from the freshly
recomputed value does not: the parts it leaves alone keep moving with the cycle, every round
differs, and salsa runs into its own `MAX_ITERATIONS` and panics. Pin a constant, or re-pin what was
already returned, and when the pinned type is a composite, cut every surface it exports, not just
the obvious one.

### Keeping project-wide work cheap

A check never names the whole project. The interface graph is walked per item, from the items a
check reaches, and `item_interface_reads`, the set of items and names an item's reads resolve to, is
a tiny tracked query whose value survives an edit that only shifts ranges.

The one map that does cover every package item is the set of conditional writers, the items that
write a top-level name other than their own, which a read with no unconditional winner joins.
`file_conditional_writers` builds a file's share by screening each item's syntax first: only an
assignment operator, `for`, a destructuring `%<-%`, or a by-reference `setnames` outside a nested
function can write a top-level name, so an item with no such token besides its own binding needs
no naming pass. Only the rest go through `item_top_level_names`. The screen may pass too many items
but never too few, and the fuzz battery checks it against naming on every generated program.

The project-wide maps reach per-file data through one tracked memo per file (`file_definitions`,
`file_conditional_writers`, `file_type_definitions` and the others `warm_project_folds` lists),
never through per-item queries. After a keystroke salsa must
confirm that a project-wide map is still valid, and it does so by checking the map's dependencies.
Read per file, that is about 500 memos on a 300,000-line project, and every one outside the edited
file passes on durability alone (see "The language server"). Read per item, it was about 56,000
memos, which cost every editor request about 35 ms right after a keystroke.

A check never reads a project-wide map either. Each lookup it makes across items goes through a
firewall keyed by one name or one item: `package_name` gives a name's winner and its conditional
writers, `interface_group` gives an item's cyclic group, and `frame_binder` gives the file-local
binder that an immediate or script read sees. The interface edges an item contributes to the group
walk (`item_interface_edges`) go through `package_name` too. A structural edit re-runs each firewall
that a check read, and an unchanged answer is backdated, so a new definition re-checks only the
items that read its name. `crates/semantics/tests/test_incremental.rs` pins these contracts.

`ry check --no-cache` never edits, so it has no use for those firewalls, and recording them costs a
cold check hundreds of thousands of salsa dependency edges it never replays. It runs on
`RootDatabase::one_shot`, whose `Db::one_shot` answer switches three lookups to a coarser shape.
A `NameLookup` reads the project-wide winner and conditional-writer maps once per reader and then
answers each name with a hash probe, instead of interning a name and going through `package_name`
or `test_name`. A frame binder comes from the file's binders, read once per reader, instead of
the per-item `item_frame_binders` memo and a `frame_binder` memo per name it does not list. And
`item_group` finds every cyclic group in one walk over the project (`project_interface_groups`)
instead of one `interface_group` walk per item; `ry check` runs that walk on a cloned handle
beside the parallel edge pass. Every read still goes through salsa, so the answers are the same
facts; only the granularity of the recorded dependencies changes. Under
`syntax::testing::enable_invariants()` each one-shot lookup is compared against the per-name and
per-item answer. Every `ry check` run is one-shot, cached or not; only the language server keeps the firewalls. A one-shot host also answers `Db::editor_facts` with false, so its item checks leave out expression and pattern types, literal expectations, loop variables, selected overloads and callee targets, which only editor features and rewrites read. `ry migrate` reads expression types for its rewrites, so it analyses on `RootDatabase::one_shot_with_editor_facts`.

A name being typed is a new top-level definition on every keystroke, so the folds that a new name
changes are kept to one pass: `package_definition_index` folds each package file's definitions
into the winners and, in the same loop, the names defined more than once. The duplicate check reads
those through `duplicate_binding_map`, a copy that is empty in a healthy project and so backdates.
Names in these maps are interned `Symbol`s whose hash was computed once at interning, so a fold
hashes four bytes per name and never rereads the spelling.

Types are hash-consed, so type equality is pointer equality and nothing is ever deep-cloned. Deep resolution
over the interned type graph is memoized per binding epoch and cuts any cycle to `Unknown`; the
decision log records that design.

### The package-load index

`ry load` reads a package through its own input, `PackageSources`: the `R/` files in collation
order plus the `NAMESPACE` and `DESCRIPTION` text, as separate fields so a `NAMESPACE` edit never
re-derives an item. The index has three layers, and each depends only on the one below:

- `file_load_index` erases one file's inline annotations, keeping every line break so line numbers
  match the file on disk, and records each item's position in that text. `file_items` then gives
  each item its content hash (of its erased text), whether it is a function definition, the names
  it writes and reads while it runs (`item_immediate_reads`, which excludes reads inside the
  functions it defines), and the S4, R6 and S3 registrations it makes. Both read one file only.
- `file_binds` reduces one file to the names it binds and how, and `package_bindings` joins those
  into which names are bound lazily: a name whose one writer is a function definition. A body edit
  leaves a file's binds equal, so salsa backdates them and the join, which is the firewall that
  keeps an edit from reaching every file's bindings.
- `package_index` joins the files and the bindings into what a plan diffs.

A reload depends on all of a package's files, so salsa would revisit every file's memos after any
edit. The loader avoids that with durability: a file starts at high durability and drops to low on
its first edit, and salsa skips verifying memos that depend only on inputs at a durability nothing
changed at. The diff then looks inside only the files whose hash changed, plus the eager code of
every file, which is all an edit can reach. Lowering a file's durability makes its first edit verify
every memo once (about 20 ms at 326,000 lines), and later edits of that file skip the check.
`test_incremental.rs` pins the per-file contract.

Before the console or `ry load` starts R for a package, it calls the loader's `prepare`, which reads
the sources and derives this whole index on its own threads while base R loads. The first
`load_all()` then waits for that to finish and only diffs.

Deriving the index means parsing every file, which is most of a cold start, so the first plan of a
package in a process caches the index across processes. The entry lives in the user's cache
directory (`$XDG_CACHE_HOME/ry/load-index`, one file per package root), and its key hashes the
`DESCRIPTION`, `NAMESPACE` and `R/` texts, the strict setting, and a fingerprint of the parser, the
analysis, the shipped stubs and the loader that `crates/loader/build.rs` computes. On a hit the
loader syncs the texts into the database without parsing them and hands the backend each file's text
instead of its tree; ry's runtime then parses only the code it lowers, one top-level item at a time,
which gives the same tree as the whole file would. Meanwhile a detached thread parses every file on
a database handle of its own, so the first reload finds the parses memoized, and an edit that reaches
the database first cancels it.

A fresh load on ry's runtime may also hold back the package's S4 definitions until first use. The
loader decides which (`DeferredS4` in `crates/loader/src/loader.rs` states the rules), and the
methods package's class and generic lookups in `crates/interp/src/methods.rs` force them on a miss;
[package loading](/reference/package-loading#s4-definitions) has the contract. When the definitions
read only S4 constructors and their own names, the loader gives the unit a cache key
(`DeferredS4::cache_key`), and `.ry_force_s4` in `runtime.R` restores the bindings a previous process
saved instead of running the definitions.

Running the definitions and using the classes goes through the methods package's native core
(`crates/interp/src/methods_core.rs`). The methods package keeps GNU R's R code and its metadata
objects (class representations, the `.__C__` and `.__T__` tables, the method tables), which stay the
only source of truth; the core reads and writes those same objects. When the methods namespace
loads, `install_fast_paths` puts a native prologue from `methods_core.R` in front of the body of each
function it covers, but only if the function's hash matches the GNU R version the prologue was
written against. A prologue either does the function's whole work, with the same result and
effects, or returns without observable effect and lets GNU R's body run, so a case the core does
not handle stays exact. When the methods package's version is the bundled one, every prologue must
install, so a mismatch stops start-up with an error; on any other version a mismatch only leaves
GNU R's body in place.
The covered functions are the object operations (`new`, the default `initialize`, `validObject`,
`is`, `extends`), the definitions (`setGeneric`, `setMethod`, `setClass`, `setClassUnion`) and
`cacheMetaData`. A prologue may also do part of the work and hand the rest to GNU R's code one item
at a time: `cacheMetaData` handles the classes and generics that need nothing or only a table merge,
and runs GNU R's loop body for each of the others, so the order of effects is GNU R's.

### Diagnostics

Diagnostics come in two layers over one source of truth:

- `parse_stage_diagnostics(file)` holds the syntax errors, the typing-directive errors, the
  refusals of malformed `#:` blocks and inline annotations, and, while inline annotations are turned
  off, a warning at each one. All of them are pure functions of the parse and `AnalysisSettings`.
- `file_diagnostics(file)` adds the naming findings (unresolved and unused names) and the type
  errors. The strict-mode findings are rendered separately by `strict_diagnostics(file)`, which is
  empty unless `file_is_strict` holds (`[check] strict` or the file's own directive).

The host, meaning the server or the CLI, then gates diagnostic classes by configuration, applies
per-file `# typing:` overrides, escalates unresolved names to errors under strict mode, appends the
lints, and honors `# ry: allow(...)` suppression comments. The server and the CLI share this
assembly in `crates/ry`, so they cannot disagree.

## The language server

The server runs on two threads. The async-lsp frontend, on the tokio thread, turns every request and
notification into a job for a single dedicated worker thread. The worker owns the salsa database
and all the documents, and the database handle deliberately never leaves it.

- **The latest edit wins.** A notification that changes an input first cancels the worker's salsa
  cancellation token and then enqueues its job, so a query in flight unwinds cooperatively. Every
  later job starts on a fresh, cheaply cloned storage handle, because a cancellation is consumed by
  whichever query it killed. A best-effort feature answers with nothing when cancelled, but an
  authoritative answer never degrades: pull diagnostics return a retryable `SERVER_CANCELLED` with
  `retriggerRequest: true`, and rename returns `CONTENT_MODIFIED`.
- **Diagnostics are pushed in two waves.** On every sync the server immediately publishes the
  parse-stage classes and the lints, tagged with the document version. Until the startup warm-up
  (below) ends, the first wave holds only the parse-stage classes, because the lints read the
  project-wide name maps. At idle time it publishes the settled full set, without a version.
  Without a check record, the settled set is a faithful superset of the first, so the second wave
  only ever adds findings.
- **The first wave can come from the check record.** At initialize the worker loads
  `.ry_cache/check.json`, computes the record's key exactly as `ry check` does
  (`check_cache::key`, with the server's own binary), and runs `check_cache::plan` over the
  project's files, reading the scripts it does not load from disk. A changed file the server did
  not load has no surface there, which plans a fresh run and restores nothing. Every loaded file
  the plan reuses gets its recorded findings, and the first wave shows them instead of the parse
  stage until the settled wave replaces them. The server analyses only the package files and the
  open documents while `ry check` analyses every file, so the two can differ in a finding that
  depends on a script, and the settled wave corrects it. Any input change (an edit, a watched-file
  event, a stub or `NAMESPACE` buffer, a file opened with other text) drops every restored finding,
  because a change to one file can change another's findings. Restoring the record for a
  300,000-line package costs about what a replaying `ry check` does, about 0.2 s. A client that supports pull diagnostics gets no pushes at
  all, and instead receives result ids derived from a content hash, with unchanged reports.
- **Idle work has an order.** At startup the worker first runs the warm-up: across all cores, it
  scans every file's `library()` and `require()` calls and applies the attached set once, then
  derives the per-file folds behind the project-wide maps (`warm_project_folds`). Every semantic
  wave reads those maps, and one thread would parse the whole project to derive them, so settled
  publishes wait for it. The warm-up names no item: a file's check names only the items its
  interface reaches, so a file opened at startup settles in about 200 ms on a 300,000-line
  package. After the warm-up the worker rescans the `library()` calls of any file whose scan a
  cancellation interrupted, because the edit that cancelled it may belong to another file and
  nothing else would rescan this one. Then it serves any settled publish it owes, most recently
  edited file first. Then it re-derives `project_global_names`, the set goto-definition and
  references consult on every library call, whenever the warm-up or a durability drop left it
  stale: salsa never backdates across a durability drop, so the fold re-runs once per opened file
  (about 20 ms at 300,000 lines) and would otherwise land on the first request. Next it lowers the
  durability of documents opened since (see below). With
  nothing else to do, it checks the rest of the workspace in parallel batches and never publishes
  that work. Idle work that gets cancelled is requeued.
- **Parallel idle work stops between files.** It runs on one cloned database handle per core, and
  each thread checks before every file whether a job is queued. Cancelling the threads' salsa
  tokens is not enough on its own, because salsa clears a handle's cancellation flag whenever a
  top-level query returns, so a flip that lands at the end of a query is lost.
- **A request preempts idle work.** A request is queued first and then cancels the idle unit in
  flight, if any, so a hover never waits for a background file to finish. A request never cancels
  another request or a sync.
- **Only package files stay loaded while closed.** At startup the server loads the files under `R/`,
  and a closed or disk-changed file stays in the project only if it is one of them. Test, helper and
  script files are analyzed while they are open, so a session never holds a file a fresh start would
  not.
- **The worker is deterministic and steppable.** Everything it sends the client goes through an
  `Outbox`, which is the socket in production and a recorder in the
  [simulator](/development/robustness/#the-language-server-simulator), and `run_job` is one step of
  its loop. Collections whose order reaches the client are ordered.
- **A coherence failure panics.** A failed document sync is unrecoverable, and a panic that escapes a
  worker job terminates the process deterministically rather than going on to serve corrupted state.
  An IDE feature lookup, by contrast, must never panic.
- **Only edited files change at low durability.** A workspace file enters the database at
  `Durability::MEDIUM`, an open document is lowered to `LOW`, and closing it restores `MEDIUM`. The
  stubs, settings, package metadata and data-file inputs are `MEDIUM` too, because a memo is only
  as durable as its least durable input and nearly every memo reads the stubs. An edit then
  revalidates every memo that reads only other files by durability alone, without walking its
  dependencies. `test_incremental.rs` pins that an edit never revisits the other files' trees or
  items.
- **The project file list is `LOW` from the start.** Salsa never backdates a memo whose durability
  drops, so if the project-wide maps started at `MEDIUM`, a file's first `LOW` write would change
  them, then every `package_name`, then every item check in the project (about 2 s on a
  300,000-line project). They read `ProjectFiles`, so a `LOW` list keeps them `LOW` throughout, and
  the first edit re-derives only the edited file's own memos. `didOpen` leaves text that matches
  the disk untouched, which would otherwise be a `MEDIUM` write that invalidates the whole project,
  and idle time rewrites it at `LOW` so that the first keystroke does not pay for that file's share.
  A setter that names no durability keeps the field's current one, so every write to a source file
  names it.
- **Positions convert only at the protocol edge**, against the target document's own text, in the
  negotiated encoding. That is UTF-16 by default, and UTF-8 when the client offers it. Signature-help
  label offsets are always UTF-16, as the LSP specification requires.
- **Stub buffers and `NAMESPACE` files are served standalone** and never enter the database. An
  `.Rtypes` buffer gets loader-problem diagnostics, semantic tokens, and goto-definition for type
  names, and a `NAMESPACE` file gets import validation.

## The start-up image

A runtime session that is not interactive and sources no profile starts from an image of the
session as it stands after `.First.sys()`, which [the CLI reference](/reference/cli#start-up-image)
describes for users. `crates/heap/src/image.rs` writes and loads the heap part,
`crates/interp/src/image.rs` saves the interpreter state outside the heap and decides when an
image applies, and `crates/interp/src/session.rs` either loads the image or starts the slow way.

- **Build output, read-only.** `ry-rcmd build-base` writes the image as its last step, into the
  base tree at `library/base/R/ry-start.image`, by running `ry-rcmd write-start-image` with an
  emptied environment. A release embeds the tree, image included. Nothing writes an image at run
  time, and nothing keys it on the run-time environment. `ry-rcmd start-image BASE --libr FILE`
  (`just start-image`) writes it again over a built tree.
- **One build.** The header carries the runtime's identity (`RY_RUNTIME_ID`, a hash of the
  runtime's sources), and an image of another build is skipped silently, so a development
  checkout starts the slow way after a runtime edit until `just start-image` runs.
- **The environment is read again on every start.** After the load, the start clears the options
  and runs the steps that read the process environment: the C-level options, `.Machine`,
  `.Platform` and the device variables, base R's profile, and the `.onLoad` hooks of utils,
  grDevices and stats, which set options from the environment only when they are unset. The
  profile's second `Autoloads` is detached, and utils' `osVersion` binding is unlocked around its
  hook, which assigns it. What the image holds is what those steps do not touch: base's
  namespace, the six default namespaces and their attachment, which read no variable outside
  `image::LOADING_VARIABLES`. A start that sets one of those runs the slow way.
- **Relocated paths.** The image records the R home it was written in (the base tree's staging
  directory) and its canonical form. The loader moves every string under either prefix to the
  directory the image file really lives in, which is where a slow start's resolved namespace paths
  and lazy-load databases point.
- **Loaded in one bulk pass, not mapped.** The heap image lists every reachable object with
  references as dense positions, and the loader allocates them all and then fills them in a second
  pass. Fixed objects (the heap's well-known objects, the primitives by index, `package:ry` and
  libR's ALTREP classes) are referred to by position, because both processes create them before
  start-up. Strings, symbols and base bindings are interned again, weak references load empty,
  and compiled closure bodies are stored as their source, since the bytecode tier recompiles them.
- **The writer refuses what it cannot carry.** Writing fails, and so does the base tree's build,
  when start-up warned (a load would not print the warning), left `.Random.seed` in the global
  environment, a frame, a C finalizer, an external pointer other than a builtin routine, a new
  record layout, an open graphics device, or a loaded shared library.
- **Damage fails before the heap changes.** A checksum over the whole file is verified before the
  heap takes any of it, and a damaged image starts the slow way with a line on stderr. A failure
  past that point (`LoadError::Partial`) would leave the heap half-built, so the start fails.

Interpreter state outside the heap is saved beside it: methods' dispatch globals, templates and
primitive method tables, the S4 extends and implicit-class tables, the loaded builtin libraries
and data.table's namespace. Per-process state, such as the command line, the temporary directory,
the RNG seed and the start time, is never in the image.

## Correctness and performance instruments

The fixture suites are the correctness contract. `crates/semantics/tests/test_incremental.rs` pins
the incrementality contracts by counting query executions after an edit, and
`crates/semantics/tests/test_perf.rs` asserts wall-time, memory, and keystroke-latency budgets
against a corpus of real files. [Testing](/development/testing) documents all of them, and
[Robustness](/development/robustness) documents the simulator, the fuzzers and the scenario tests
that check the whole toolchain against from-scratch runs and GNU R.
