# semantics

Read this before you change the salsa analysis core and the type checker. Cross-cutting traps live in `.agents/memory/MEMORY.md`.

## Incremental analysis

The analysis core is incremental. `semantics` is a salsa database, so queries are memoized and an edit cooperatively cancels analysis that is still in flight. Per-item interface firewalls keep an edit inside one item from invalidating the rest of the project, and the LSP server publishes a fast diagnostics wave first and schedules the slower semantic wave for idle time.

The architecture page at `docs/src/content/docs/development/architecture.md` is the contract. Read it before you touch the analysis core or the server's scheduling, and keep it accurate. Deferred performance work lives in `.agents/memory/backlog.md`.

A check must never read a project-wide map directly, because then every edit anywhere would invalidate it. Go through a per-name or per-item tracked projection instead, and pin the property in `crates/semantics/tests/test_incremental.rs`.

## Traps

- **A per-item query never scans a per-file or per-project list**: compute once per file in a memoized query and probe per item (`item_spans` is the one source of item identity). Vary both items and names when measuring.
- **A per-item read that joins over a project-wide list needs a cap, not just memoization** (`CONDITIONAL_SLOT_JOIN_CAP`, widening to `Unknown`).
- **Never call `Checker::infer` twice on one expression.** It has no memo, so a double walk costs 2^depth on operator chains and repeats findings, which the deduplicating fixtures hide.
- **Hot tracked queries returning maps or vectors use `returns(ref)`**, and `GlobalEnv` facts are borrowed `&'db` values; `returns(clone)` deep-clones per call.
- **A type is a hash-consed DAG: walk it with a memo, never as a tree** (`substitute_rigid`).
- **A `Ty` and a `Symbol` are ids in interning order, which differs between runs of a parallel check.** Nothing that reaches output may walk or sort by `Symbol::index()` or a type's id; `Symbol` orders by spelling for that reason. Ask a type's id bits (`may_resolve`, `contains_inference_var`) before walking it, and add no cache in front of the interners: a lookup takes no lock (`.agents/memory/decisions/syntax-semantics.md`, "Decision record: types and symbols are four-byte ids into append-only columns").
- **A reader of an item's syntax depends on `item_stamp` alone, through `item_syntax`.** Reading `item_spans`, `item_node` or `item_name_range` in a per-item query ties it to every edit of the file. Never put the stamp in a value another item reads, such as `item_syntax_flags`, or a body edit re-runs that item's readers (`.agents/memory/decisions/syntax-semantics.md`, "Decision record: an item's syntax has a stamp, so an edit lowers only the items it changed").
- **A composite type past `TYPE_SIZE_CEILING` records as `Unknown`** (`Checker::record`), or a record whose fields return that record grows tenfold per level. `types::type_size` counts paths, not nodes.
- **A query that reads a syntax tree keeps a small memoized result and is warmed in `diagnostics::derive_from_syntax`**, or it walks other files' trees on the checking threads (`.agents/memory/decisions/syntax-semantics.md`, "Syntax trees are evicted, and only small syntax facts are memoized").
- **Every host runs the same queries; never add a lookup that only the command or only the server takes.** A host chooses the capacity of its `TreeStore` and `Db::editor_facts`, nothing else. Syntax trees live in that store, so no query may memoize a file's tree or an `ItemSyntax` window (the detached subtree of an annotation is the one syntax a memo keeps), and a command's pass that derives from a file's tree holds the file (`TreeStore::hold`) (`.agents/memory/decisions/syntax-semantics.md`, "Decision record: every host resolves names through one path, and trees live in the host's store").
- **A check reaches other items through `item_links`, the per-name queries and `file_interface_groups`.** A new per-(item, name) memo for names nothing binds costs about 100,000 memos on a 300,000-line project, so answer from the layer of the item's file first (`package_name`, `test_name`), as `SalsaGlobals::unlinked_bindings` does, and never from a fold over every file.
- **Editor budget: keystroke-to-diagnostics p50 at most 30 ms and p95 at most 100 ms at 300K lines** (`test_perf.rs`, `just perf`).
- **`GlobalEnv` has no default method bodies**: the SCC fixpoint's view is a second implementor, and a default silently gives it a wrong answer. Test with a recursive and a non-recursive fixture.
- **An immediate read resolves to the nearest earlier writer in the file; a deferred read (a closure) sees the project-wide winner in a package and the settled file frame in a script** (`frame_binder`).
- **A top-level loop variable stays a `ForVariable` binding** (`Naming::loop_bound_slots`). As `BindingKind::TopLevel` it becomes a global item, which breaks the unused check, goto-definition and rename.
- **A read recorded by name instead of by expression id is invisible to the slot model**, so `%op%` reads go through `mark_slot_read` (`naming.rs`). Implicit reads for `unused` stay in naming's slot reads (`mark_formals_read`), never in `captured_slots`, which the checker types with.
- **`"x" <- 1` binds `x` through the name-target arm, not `replacement_base`**, which reads the base first.
- **A called name is looked up as a function** (`Checker::function_lookup`), and editor features follow `ItemCheck.callee_targets`, not slot resolution (`.agents/memory/decisions/syntax-semantics.md`, "Called names are looked up as functions").
- **Dispatch and the numeric constraint read the same class scope** (stubs plus project), so a package's own `+.Money` makes `Money` arithmetic.
- **Test files are `DocumentKind::Test`, helper and setup files `TestHelper`**: only helpers fill the shared test namespace, and no test binding reaches the package (`.agents/memory/decisions/syntax-semantics.md`, "testthat files mirror testthat's environments").
- **HIR lists are `Span`s into the module's side arrays and every HIR kind is `Copy`** (`hir.rs`). Literal text is a `Text` span, never a `Symbol`, because the interner never frees. Lowering visits arguments before the callee, and per-expression maps rely on that `ExprId` order.
- **One structural map rewrites types: `types::map_child_types`.** New `TyKind`, `FunctionType` or `Parameter` members go there only, because a second hand-written walk misses one.
- **Every type the checker prints must parse back** (`test_roundtrip.rs`), so a new constraint or `TyKind` includes its grammar side.
- **Every mismatch report goes through `Checker::type_mismatch`**, which narrows to the failing field or position. A finding underlines the smallest expression its message is about (`name_range`).
- **Every finding inside an item is reported; only the item's export is cut to `Unknown` when it fails.**
- **Do not `table.resolve` a type you display as written** (it expands aliases), and use one `TypeRenderer` across everything that shares variable names.
- **A `#:` annotation declares parameter types, never the parameter list.** The export is always the formals, and `check_declared_function` reconciles, so a call is never blamed for an annotation's mistake.
- **An inline type definition pairs with a `#:` definition directly above it**, so read definitions through `file_type_definitions`, never by walking the tree for one spelling.
- **Stubs are declaration-only `.Rtypes` files in `types/`, a set-once input that project `stubs/*.Rtypes` files override**; an overload set ends with an `Any` fallback (`stubs.rs`).
- **Export manifests (`types/*.exports`) come from a live session** (`Rscript scripts/export-manifests.R`) on an R at least as new as the recorded one, or real exports become false `unresolved` findings.
- **The fuzz batteries' cost is re-parsing the stub corpus per fresh database**, not the iteration count.
