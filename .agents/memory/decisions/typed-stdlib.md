# Decisions: Typed ry and the standard library

Typed closures and their native compilation, records, tuples and tags, and `std`.

## Beta semantics: Signature matching is name-aware

**Status:** built.

An R call site matches arguments by name, so matching an annotation against a definition by flat position routes a value to a wrongly typed formal and accepts code that crashes at run time. Contract and implementation match by name where names exist.

## Records, tuples and tags are ry values, not R lists

**Status:** built, except the lowercase `record{...}` and `tuple{...}` spellings, which "The DataFrame API is Polars', and ry's own types are UpperCamelCase" supersedes.

- **Shape.** One heap object kind of ry's own (a type code GNU R leaves unused) holding a 32-bit interned layout id and eight-byte slots. The layout is keyed on the tag (an interned small integer) and the canonically sorted field names only; which slots hold raw scalars is a per-object mask, so same-typed values share a layout id. Values are immutable with value semantics. Draft: `docs/src/content/docs/development/design/records-and-tags.md`.
- **Why not R lists.** A plain or classed list would make R's list semantics (attributes, partial `$` matching, `c()` coercion, `names<-`) the semantics of ry's types and give the JIT no fixed layout.
- **Types.** Tags are structural; a tag union is an ordinary union, `..` makes it open, and there are no row variables, which keeps the checker Hindley-Milner.
- **R boundary is explicit.** `as.list()` converts out, `as.record()`/`as.tuple()`/`as.tag()` in. Implicit classes `ry_record`, `ry_tuple`, `ry_tag` serve `class`, `inherits` and S3 dispatch. Printing, `identical`, `all.equal`, field access, type predicates and builtins that only store a value accept native values; every other builtin raises one error naming the conversion. C sees an opaque object (`TYPEOF` 27): list element moves work and every other accessor raises the same error, because a deep scan at `.Call` would cost time proportional to the data. `serialize` refuses them.
- **The refusal is a builtin flag, not a scan.** `BuiltinDef::accepts_native` is set from `syntax::native_values::ACCEPTS_NATIVE`, and `install_all` asserts every listed name exists, so a renamed builtin cannot silently drop off. Only top-level arguments are tested. The checker reads the same list (it lives in `syntax` because `semantics` and `interp` both depend on it) and reports a native value passed to an unlisted R function as `type-mismatch`, only when every member of the argument's type is the same native kind and never for a project-defined or shadowed function.
- **Raw slots under NaN prefixes.** A raw integer or logical sits under its own quiet-NaN prefix, so a one-bit mask suffices and a slot read needs no layout lookup. The slot count lives in header padding, so the collector never reads the layout table.
- **Compiled sites.** `x$name` compiles to `bytecode::Op::Field` with a per-site `[layout id, slot]` cache (`records::cached_field`, `records::NO_LAYOUT` until seen); any other object takes `$`'s own path. The JIT turns a filled cache into `Intrinsic::Field` (guard kind, no object flag, layout id; load unboxed when the mask bit and NaN-box prefix match feedback). `$` had to join `base_primitive`'s list, or every site compiled as a general call. `x[[k]]` with constant `k` is `Op::Item` with the same cache; constructor calls with distinct names intern their layout at compile time (`Op::Construct`), and a payload-free tag is one shared constant. A guard-free `match` compiles to shape, tag, literal and slot tests, binding only after the whole pattern matched, as the special does.
- **`$<-` on a record keeps its type**: writing a field checks the value, and writing a missing field is an error, because the runtime layout is fixed by the field names.
- **Pattern encoding reuses the value constructors** (`.ry_tag`, `.ry_tuple`, `.ry_record`), so a quoted pattern deparses like a quoted value; `_`/`..` stay bare symbols.

## Inline annotations at run time

**Status:** built, except GNU R through `ry run` and `ry repl`, which "ry run and ry R: the command chooses the runtime" supersedes.

ry's runtime checks a function's inline parameter and return types at its boundary and erases binding annotations. The contract is the typing reference, "Annotations at run time".

- **Why check.** The checker trusts an annotation at every call site in other files, so an unchecked wrong annotation can stay false forever; a boundary check turns it into an error at the call that broke it. It also gives the JIT a guarantee inside the body.
- **Parameters are checked when forced**, because checking on entry would force every annotated argument and change R semantics (laziness, `missing()`, `substitute()`). The promise carries `gp::CHECKED_PROMISE` and `force_promise` checks before storing: a failed check leaves the promise unforced and still checked, so caught errors cannot expose an unchecked value. A promise forwarded through `...` is pending in several typed frames and forcing it checks every one; the bit clears only once no frame holds it. An already computed value is checked on entry.
- **Storage masks.** A check is one bit test of the `SEXPTYPE` against a `u32` mask (`crates/interp/src/annotations.rs`). Each mask is at least as loose as the static type (vector shape ignored, numeric promotion admitted, alias, class, type variable, `Any` or frame unchecked), so a value the checker accepts never fails at run time. Element types and attributes would need a walk, too costly per call.
- **Where the signature lives.** Lowering a `function` registers masks keyed by the formals pairlist (or the body, without parameters), which closures share by pointer, so `environment<-` copies keep checks and `formals()`, `body()`, `deparse()` see no change. A closure matches only when its body is the definition's body object, because `rlang::new_function(formals(f), body)` would otherwise inherit `f`'s checks. The key carries `gp::TYPED_DEFINITION`, so an untyped call pays one header bit test. Entries are weak references whose key identity guards against address reuse; dead entries are swept when the map doubles. A parameterless function whose body is a constant or symbol has no unique key and is not checked.
- **Bindings are not checked**: `x: T <- v` would need a check per assignment and a reassignment policy, and the checker already reports a wrong value statically.
- **The JIT** may skip a check at a call site whose argument types the checker proved, because the mask test is pure. The pending-check stack (`annotations::State::pending`, popped by frame depth) is the only state a compiled frame honors.
- **Opt-out.** `[check] inline-annotations = false` erases annotations and registers no checks (`RuntimeFlags::annotation_checks`), so the setting means the same in analysis and at run time. `ry run`/`ry repl` preflight the code as a one-file project (`crates/ry/src/preflight.rs`): they print `inline-annotation` and `annotation-mismatch` warnings and refuse in strict mode; other findings stay `ry check`'s, and a script with no inline annotation and no strict mode skips analysis.
- **The console sees whole commands.** The filter holds an unfinished command and releases it whole; "unfinished" is `syntax::input_is_unfinished` (every syntax error marked `incomplete` by the parser, which a range check cannot know because an error at a trailing newline is reported on the last code character). Otherwise strict mode could refuse the last line of a definition R had partly read.
- **The console preflight checks each command against the session's interface** (`semantics::session`). Each accepted command is its own `SourceFile` with `command: Some(index)`; a singleton salsa input log (`ConsoleSession`, grown by `commit`, shrunk from the end by `truncate`) records what each command binds, declares and defines, its `# typing:` directive and whether it used ry syntax. Lookups for earlier commands fall back to the log, untracked on purpose: they read only smaller indices, and the log changes only past the reader or by dropping later commands. Re-analysing one growing session file was O(n²) per paste.
- **Function bodies see the latest binder**, as R's global environment does, through a `SessionBinder` input per name created on first such read. `commit` returns earlier commands that read the name, transitively through function bodies (a caller's type follows its callee's); strict mode rechecks those that passed when they joined, a strict error refuses the new command, and `truncate` undoes the commit. Tracking immediate reads too would make each rebinding invalidate a chain and bring the quadratic cost back. Command files are not in `ProjectFiles`, because a fold over them costs O(session) per command.
- **Rollback.** Only Ctrl-C drops input (R gets one line per chunk). `ConsoleFilter` maps dropped lines back to the command's bytes (identity for ry's runtime; `syntax::erased_lines_end` for GNU R's erased program), and `Preflight::retract` replaces the last command with the prefix that ran, unchecked. A `# typing:` directive governs only later commands.

## Ry's standard library (`List`, `Str`, `Num`, `Result`, `Dict`, `Set`)

**Status:** built, except the `ry` namespace as the library's home at session startup, which "The standard library is the `std` namespace, with an optional prelude" supersedes.

- **Names.** Plain qualified names such as `List.map` are ordinary R symbols with a dot. There is no static dispatch, so a call is a symbol lookup and the checker reads one stub per name. User code shadows a library name like any R binding, and `test_standard_library.rs` asserts no shipped package's `.exports` holds a library name, so plain R cannot change meaning. Module names are Roc's; a Roc name that means something else in R is renamed (`Result.try` is `Result.and_then`).
- **One source of truth per side.** Types live in `types/std.Rtypes`; runtime functions in `crates/interp/src/standard_library.rs`, registered with `BuiltinDef::accepting_native`. A test asserts the name sets are equal and every declared parameter name is accepted at run time.
- **`Dict` and `Set` are native kinds** (`Fields::Dict`, `Fields::Set` in `crates/heap/src/records.rs`) with value semantics, insertion order and an open-addressing index, because a named list matches partially and allows duplicates and an environment has reference semantics. `identical()` ignores order, as Roc's equality does.
- **Errors are tags, not conditions.** A fallible function returns `:Ok(x) | :Err(:Tag)`; a wrong argument type raises an R error naming the parameter, so misuse is loud and a domain failure is a value.
- **Docs.** Contract `reference/standard-library.mdx`; open questions `development/design/standard-library.md`.

## Standard library updates reuse a uniquely held value

**Status:** built.

- **Shape.** Perceus-style reuse, as in Roc and Koka. `eval::rebind_call` recognizes `x <- f(x, ...)` for a builtin flagged `reusing_first_argument` and passes the first argument as `Call::reusable` when the frame binding holds its only reference and no other argument is the same value; `standard_library::owned` also accepts a refcount-zero temporary. Dicts and sets are counted records with resizable key and value vectors that double when full. Without this a loop growing a collection is quadratic.
- **Why not a special form or last-use analysis.** A special form would make every library update syntax, and last-use analysis needs the compiler; the reference count is exact at the call boundary for AST frames. It is not exact for a frame a compiled closure owns, because bytecode registers hold uncounted references; see "In-place updates through nested targets".
- Value semantics are unchanged: any second holder copies.

## Standard library breadth against Roc's builtins

**Status:** built, except the skipped wrapping arithmetic and the R-sized limits and conversions (`Num.max_integer`, `Num.to_integer_checked`), which "Typed ry has its own numbers, `Bool`, `Str` and `List`" supersedes.

- Each module was compared with Roc's builtins (pre-`Try` API) and everything typed, `NA`-free and reporting failure as a tag was taken; the list and renames are in `development/design/standard-library.md`.
- **Skipped:** `Box`, `Encode`, `Decode`, `Hash`, `Inspect` and the `Eq` implementations (they serve Roc's layout or abilities; `Bool.is_eq` compares by `identical()`); effectful `!` variants (R tracks no effects; `List.for_each` covers them); `Str` capacity functions (R strings are immutable and interned); Roc's `nan_f64` (nothing takes or returns `NaN` on purpose).
- **Capacity functions are taken** (`Dict.capacity`, `release_excess_capacity`, `with_capacity`), because the heap never shrinks a resizable vector, so room after `reserve` stays until the value dies. An empty collection has no key vector until its first key fixes the key type, so its index carries the requested room (`standard_library::empty_room`) and the first insert allocates with it.
- **`NaN` is never a result.** An unchecked function that would produce `NaN` or overflow (`Num.sin(Inf)`, `Num.pow(-1, 0.5)`) raises an R error naming the `_checked` variant, which returns `:Err(:Undefined)`. Unchecked operations are kept so a step can be passed by name (`List.walk(xs, 0, Num.add)`). `Num.is_nan` is the one function that accepts `NaN`.
- **`Bool` is taken** so boolean steps can be passed by name. `Bool.true`, `Num.pi` and the other constants are values in `standard_library::CONSTANTS`; under GNU R, `crates/ry/src/load.rs` binds each non-function `std.Rtypes` declaration as an active binding that stops with the "only ry's runtime" error.
- **UTF-16 and UTF-32** follow Roc's newer `Str` builtins (`:BadUtf16`, `:BadUtf32`); NUL is bad because an R string cannot hold it.
- **Hover docs** come from `#'` lines above a declaration in a `.Rtypes` file (`StubLibrary::documentation`); a later source's first declaration of a name replaces or clears them, like the type.

## The standard library runs only on ry's runtime

**Status:** built, except the check-time diagnostic for library calls in a project that targets GNU R.

- A pure-R build for GNU R was declined: many functions return result tags and `Dict`/`Set` values GNU R cannot represent, and a partial `Str`/`Num` build would split the library into two dialects.
- Instead, a diagnostic should flag library calls once a project can declare that it targets GNU R (`backlog.md`).

## Nominal and structural types cover both product and sum types

**Status:** built.

- **Shape.** Every record, tuple or tag union is structural (`alias` only names it), and `type Name = ...` (or `#: @type`) wraps it in a nominal type: one mechanism gives the 2x2 grid. Contract: the reference section "Nominal records, tuples and tags".
- **Construction.** `@Name(...)` is the only constructor and its parentheses are the value's own (`@Point(x = 1, y = 2)`, one-field `@UserId(id = 1L,)`). A bare value of the right shape is refused, keeping construction in one checked place like Roc's opaque `@Name`. `Point(x = 1)` was rejected because it collides with R calls.
- **Projection.** Exhaustiveness, pattern checks, field writes, `$` completion and the native-to-R check project the nominal value to its representation through the visible type definitions (`exhaustiveness::structural`, `Checker::structural`, `ide::representation`), so the definition is the single source of truth for the shape. Binding patterns keep the nominal type. Exhaustiveness splits on constructors only while a row has a constructor head, which lets a recursive `type Tree<T>` terminate.
- **Tests:** the `nominal_grid` typing group, `crates/ide/tests/ide/records-and-tags.R.test`, `crates/interp/tests/test_records.rs`.

## An Array is a flagged R vector, written `[...]`

**Status:** built, except the type name `Array<T>` and its R item types, which "Typed ry has its own numbers, `Bool`, `Str` and `List`" supersedes.

- **Shape.** `[a, b, c]` is a strict vector: one exact item type, no `NA`, no attributes, no recycling, no implicit conversion. Contract: `reference/type-system.mdx`.
- **Runtime representation.** An ordinary R vector carrying `flag_bits::ARRAY` (`crates/heap/src/object.rs`): atomic storage for scalar items, a VECSXP for ry-value items. A separate object type was rejected because every C API accessor, `sum()` and the JIT already read unboxed storage, so converting to R is dropping the flag. Setting any attribute clears the flag, keeping "no attributes" true without checks at every attribute write. `values::duplicate` and `shallow_duplicate` drop the flag on the copy, because a copier is about to write and may write an `NA`; only `arrays::writable` re-flags a copy after checking the write.
- **One place enforces the rules**: `crates/interp/src/arrays.rs` (operators via `strict` wrappers in `arith.rs`). Bytecode fast paths and the JIT's type guard refuse a flagged operand, so they can never skip a check.
- **R code keeps working on Arrays.** Reads follow R's positions wherever R gives no `NA`; anything producing `NA` is an error. A replacement or operand that is not an item of the item type yields the plain R vector R would build (`arrays::check_operands`), because base functions such as `ifelse()` assign into an argument and `str()` computes `ob - signif(ob, digits)` on a plain vector. Typed code cannot reach that path because the checker rejects the value.
- **`List` results** are Arrays only when the collection argument is one. A callback's result type is bound by the `item` constraint in `types/std.Rtypes`, so the checker rejects mapping to a non-item.
- **The flag is the single source of truth for strictness.** Tests: `crates/semantics/tests/typing/arrays.R.test`, `crates/interp/tests/test_arrays.rs`, Array seeds in both fuzzers.

## Array storage stays row-major; record Arrays go columnar only with JIT field fusion

**Status:** built.

- **Scalar Arrays already have the data-oriented layout**: one contiguous NA-free buffer.
- **An Array of records stays a list of record objects.** A columnar shape would save per-item headers and collector work, but every `xs[[i]]` would allocate a record to hand out and `.Call` code reading `VECTOR_ELT` would need a materialized copy. It pays only once the JIT fuses `xs[[i]]$field` into a column load, and DataFrame already serves columnar records.
- **Trigger to revisit.** When the JIT compiles `xs[[i]]$field` on Arrays, measure a million-item record Array row-major against columnar (`backlog.md`, "Columnar record Arrays") and switch only if it pays.

## In-place updates through nested targets

**Status:** built.

- **Nested targets.** `rebind` accepts a target that is a chain of `$` or `[[` with a constant name, string or number (`same_expression`), so `state$seen <- Dict.insert(state$seen, k, v)` updates in place. `solely_held` requires one reference at each plain list on the chain, and no other argument may be the value or a list on the chain, because argument values are not counted (`holds_on_chain`).
- **Bytecode.** `CallRebinding` carries the target so compiled code takes the same path. It is appended last because `OPS` is indexed by discriminant. The JIT inlines it only when the target's slot type is plain.
- **Register rule.** Registers hold uncounted references, so an update from outside the body (`eval.parent()`, a forced promise) could change a value a register is about to use. `gp::COMPILED_FRAME` marks an environment while a compiled body runs on it, and `reuse_permitted(env, value)` allows an update when the flag is clear, when the innermost frame is that environment's own compiled closure (whose updates the compiler orders with `may_modify_in_place`), or when no live register holds the value. A stale register only costs a copy. This applies to `complex_assign` too, so it fixes plain R as well.
- **Record fields.** `r <- (..r, field = f(r$field, ...))` lowers to `.ry_update(r, ...)`, which `rebind_update` evaluates with `r$field` as a nested target. That needs records to count holders: interpreter-made plain records are counted (`records::allocate`, `set_record_slot`), tuples and tags stay saturated. Fields after the updating one must be constants, because their code could otherwise see the field half-updated. In a slot frame the compiler leaves such an update to `CallRebinding` (`eval::updates_own_field`).
- **Collection storage stays as it is**: a dictionary or set is already struct-of-arrays in the collector's heap, which a `SEXP`-holding value must be. Hashing and probing were at most about 15% of a call; the rest was call overhead.
- Value semantics are pinned in `crates/interp/tests/test_standard_library.rs`.

## Library leaves resolve at compile time

**Status:** built.

- **Tier 0.** The bytecode compiler resolves a qualified library name (`library_function` in `bytecode.rs`) to its builtin when it is native-accepting and every argument is positional with no `...`, emitting a guard on the symbol's binding and a direct `CallBuiltin`. A failed guard (someone rebound `Dict.len`) falls back to the general call.
- **Leaves.** `standard_library::is_leaf` names functions that never call back into R: every `Result`, `List`, `Dict`, `Set` function without a callback that does not update its first argument in place, and every `Num`, `Bool`, `Str` function except the `Str` walks. The JIT treats a leaf as `Effect::PureUnlessObserved`: it flushes nothing unless an argument's object or ALTREP flag is set, because reading an ALTREP vector runs its class's methods, which a package may write in R. In-place updates are excluded because compiled code may hold the argument in an uncounted register. `leaves_are_the_functions_without_callbacks` derives the rule from the stub signatures.
- **Intrinsics.** `Dict.len`, `Set.len`, their `is_empty`, `contains` and `Dict.get` call small runtime helpers (`jit_collection_count`, `jit_collection_position`, `jit_dict_get`) that take the key unboxed; anything unhandled (wrong type, `NA`, mismatched key kind, an index-less one-entry collection) hands the instruction to tier 0, which raises the same error. `:Err(:KeyNotFound)` is one shared value (`standard_library::Tags`).
- **Hash.** `key_hash` applies MurmurHash3's 64-bit finalizer after FxHash, because FxHash's low bits are constant for whole-number doubles and the index picks slots by low bits, so integer-valued keys clustered. The index is not serialized, so the hash can change freely.

## Library traversals compile to loops

**Status:** built.

- **Interpreter.** `standard_library::Callback` checks a closure callback once per library call: with no `...` and exactly the arguments the library passes, items bind positionally (`bind_arguments`, `run_closure`), skipping argument matching. The frame's call still quotes the values, `(f)(5)`, as `eval::apply` does; a call naming the formals, like GNU R's `FUN(X[[i]], ...)`, would be wrong, because `sys.call()`/`match.call()` users evaluate it in the caller's frame, where formal names find the caller's variables.
- **JIT.** `call_builtin` turns the looping library functions into `Action::Traverse` when the callback inlines (a closure literal, a function constant, or a name whose lookup finds a closure now). The element type is the join of the callback's own read feedback (or a declared scalar annotation), and the body lowers through `lower_expression`, so it must be scalar arithmetic, comparisons and intrinsics.
- **One loop, described by a struct.** `analysis::Traversal` is a fold (`Walk { until }`, `Map`, `Filter`, `Any`, `All`, `CountIf`, `Find { index }`), a source (`List`, `Dict`, `Set`) and flags (`backwards`, `indexed`, `from`); `Traversal::of` decodes every looped name, so a new variant is a combination, not a code path. `walk_until` bodies lower through `lower_step` into a (break, value) pair; other body shapes refuse.
- **Guards.** The loop hands the whole call back to tier 0, which reruns it from the start, when the list is not a plain `VECSXP`, an element is not the expected unboxed scalar, a predicate returns `NA`, or arithmetic overflows. A hand-off before anything visible is written is invisible. A named callback adds an identity check plus a speculation on its binding that keeps the closure alive (so a recycled address cannot pass) and recompiles when the binding changes.
- **`if` in inlined bodies** lowers to `Action::Select`, evaluating both arms, which is sound because an inlined body has no effects; an `NA` condition hands off. A formal whose value cannot be unboxed binds to `UNUSED`, so a body reading it refuses rather than resolving to a caller variable.
- **Tests.** `traversals_agree_across_tiers` and `fuzz_stdlib_traversals_agree_with_model` in `crates/jit/tests/test_tiers.rs`.

## The standard library is measured against GNU R, function by function

**Status:** built.

- **Benchmark.** `just bench-stdlib` (`tools/bench/internal/stdlib`) times every `List`, `Str`, `Dict`, `Result` function against the fastest of base R, purrr, stringr and stringi, with checksums. A row slower than R is fixed or listed in `backlog.md` ("Open: standard library rows slower than GNU R") with its reason. Where the input is a vector, ry's side also runs on an Array, because R's vectorized functions work on unboxed storage too.
- **Accessors read in place** (`list_count`, `list_item`, `list_slice`), so accessors, `take_*`/`drop_*`, `sublist`, `split_at` cost what they return. `List.contains` compares a number by value, which is what `identical` decides for a non-`NaN` number; a length-one Array is never such a plain scalar, because `identical([1], 1)` is `FALSE`.
- **Leaf callbacks skip the call.** A leaf builtin callback is called through its `BuiltinFn` (`Callback::leaf`, `call_leaf`); `call_leaf` swaps in the call `eval::apply` would have built only when an error escapes, so the condition is unchanged.
- **Updates in place.** `Dict.update` and `List.update` reuse their first argument and re-check ownership after the callback returns, because the callback may have stored the collection. `List.update` holds a reference while the callback runs, so a `<<-` into the list copies it, and a callback returning the list itself takes the copying path so the list never holds itself. Removal through `Dict.update`/`Dict.remove` rebuilds, because a `Dict` keeps insertion order.

## Moving a collection into a helper's parameter

**Status:** built.

- **Shape** (`crates/interp/src/moves.rs`). `rebind_call` offers `d <- helper(d, k)` to `moves::call_moving`. A static check (`movable`) requires positional arguments without `...`, and that the other arguments, the callee's defaults and its body call only closures and inert builtins (`inert_builtin`, plus native-accepting library builtins), never naming the caller's variable unless it is the callee's own local. The caller's binding is then `NULL` for the call. At run time anything the check did not cover ends the move first and restores the binding: entering another closure, forcing an unseen promise, a non-closure library callback, the default error handler. Clearing the binding without restore was rejected: a second argument naming `d`, a nested closure, `get(..., parent.frame())` and an unwinding error all read the cleared variable.
- **Undo journal.** An in-place dict/set update either appends or replaces (`insert_in_place` reports replacements to `moves::replacing`), so the original is the first `entries` entries with replaced values restored (`collection_prefix`). A plain list moves the same way (`moves::extent`; `List.set`/`List.swap` report replacements); growth past capacity copies.
- **Dead frame release.** When the callee returns without the move ending and creates no escaping closure, `release_parameter` clears the parameter's binding; otherwise the dead frame keeps the result shared and the next call copies. The heap does not count an environment's enclosure as a reference, so the environment's refcount cannot decide this as GNU R's `R_CleanupEnvir` does.
- **Holes the static check cannot see.** An environment-valued variable can reach any frame, so `$`, `[[` and their assignments on an environment end every move (`moves::reaching_environment`). A local assignment of the caller's variable name does not shadow it (a read before it, or on a path skipping it, finds the caller's variable), so such a body is not movable. Closures created in the body are recorded (`moves::created_closure`, also for `values` duplicates), and release and tail update require each to be referenced only from the move's private frames (`closures_dead`). `movable` accepts a `function` literal whose defaults and body pass the same check (`check_literal`); entering such a checked closure does not end the move, and its frame joins the tracked frames.
- **Base replacements end the move.** `xs[[i]] <- v` and kin modify in place without the journal, so `subset::modifiable` and complex assignment call `moves::modifying` first, before the sharing check, because ending the move makes the object shared and forces the copy.
- **A shared start does not stick.** A value something else holds (`.Last.value` after a top-level `d <- Dict.empty()`) cannot move, but the call runs tracked (`Move::shared`) so the dead frame is released and the next call moves.
- **Tail update.** `f(p, ...)` in tail position, `f` a reusing library builtin and `p` the first parameter, compiles to `Op::CallTailUpdate` (`bytecode::tail_updates`). It updates in place only when a move into this frame still lasts after argument evaluation and every closure created meanwhile is dead with the frame and absent from the call's other arguments (`moves::frame_private`). The AST evaluator never does this, so the calls before tier 0 compiles the body copy.
- **Slot frames.** A move names its binding as `moves::Binding` (a cell, or a slot) and the caller's names as `moves::Heads`; `op_call_rebinding` hands the call to `moves::call_moving_from`, refused while a register of the calling expression holds the value.
- **Why static types do not prove it.** A type says what a value is, not which frames a function reads or whether it captures its environment; effect annotations would be the typed route, and ry has none.
- **Limits.** The tail update covers only the last statement and is off in a body that may capture its frame (`backlog.md`).

## A key of the wrong type is an error, not a missing key

**Status:** built.

A dictionary or set holds keys of one kind. A lookup with another kind stops with an error through the same `joined_key_kind` check insertion uses, because returning `:Err(:KeyNotFound)` hid a bug the checker reports statically. Integer and double keys join as numbers, and an empty collection has no kind yet, so any key misses.

## Records, tuples and tags compile natively

**Status:** built.

- **Construct.** `Action::Construct` writes `(kind, bits)` pairs into a stack buffer (`runtime::representation`'s encoding) and calls `runtime::jit_construct`. Unboxed scalars go into unboxed slots; a double whose upper word collides with an integer or logical prefix stays a handle.
- **Guarded match.** The guard compiles in place with bound names read from registers, and `Op::MatchCondition` applies the special's condition rules. Anything that would reach the frame by name evaluates in a scratch child frame holding the bindings (`Op::BoundEval`). A guard that assigns, creates a closure, nests a `match`, leaves a loop or calls a bound name (`guard_compiles_in_place`) runs whole through `Op::BoundEval`, so an assignment in a guard never reaches the function's frame.
- **`MatchFail` always signals**, so the JIT treats it as `PureUnlessObserved` and leaves every register `Bottom` after it; as an opaque step its stale register types widened the join after each `match` and pushed loops to tier 0.
- **List element reads** speculate on site feedback like tuple items and record fields.
- **`List.range` and `List.repeat` stop past 2,147,483,647 elements** (R's longest integer-indexable vector), so a typo cannot request an unbounded allocation.
- **Benchmarks.** `tools/bench` `#@ typed` / `#@ untyped` variant blocks let the finance `ledger`, data-science `readings` and biostatistics `followup` templates use records, tags and tuples when typed and `list(kind=)` when untyped, drawing the same instances.

## The standard library is the `std` namespace, with an optional prelude

**Status:** built.

- **Shape.** The library is its own namespace, `std` (`types/std.Rtypes`, `types/std.exports`); `ry` keeps the DataFrame API. `std::Name` always resolves. `[check] prelude` (default `true`) also puts bare names in scope; `false` makes a bare name unbound (with a fix to `std::Name`) unless the project defines it. Types (`Result`, `Dict`, `Set`) stay in scope, because they are type syntax, not namespace members. This is Rust's `std::prelude` shape, which fits because R already reads `pkg::name` as namespace access; Roc has no opt-out and Gleam no prelude.
- **Incremental.** The checker gates bare lookups through `stubs::outside_prelude`, which reads the setting only for library names, so flipping it rechecks only items using the library (`test_incremental.rs`).
- **Runtime.** It binds both namespaces and attaches `package:std` only with the prelude on; the host passes the negative flag `RUN_FLAG_NO_PRELUDE`, so `R CMD` and older hosts keep the prelude.
- **Docs are generated.** `crates/semantics/tests/test_std_docs.rs` renders `reference/std/` from `types/std.Rtypes` and fails when committed pages are stale (`just docs-std` rewrites them), so the declarations are the single source for checker, hover and docs; a hand-written copy drifted.
- **Gap.** Compiled code fast-paths only bare library calls, so `std::List.map` is not fast-pathed (`backlog.md`).

## Typed closures run as native functions over records, tags and match

**Status:** built.

- **Call-time entry.** `jit_call` in `runtime.rs` (from `apply_closure`) runs a typed closure (`annotations::is_typed`) as a native at its first call. Arguments must be positional or name-exact, and each is read without forcing (a constant, a forced promise, a promise of a constant, or of a symbol bound to a plain value). Its `Ty` comes from the value; the native is cached per closure and `Ty` list, and a trampoline per machine signature passes words and boxes the result. Debug, step, trace and move tracking skip the path.
- **Records as pointers.** `Ty::Rec(layout)` holds a record pointer; distinct layouts join to `Ty::Obj`. Field reads speculate from per-layout slot-kind profiles the heap records at allocation (`heap::records::slot_kind`); an empty profile fails the read and a mixed one refuses the native.
- **Construction and match.** `jit_native_record` builds from a stack array; zero-slot tags are shared singletons. `match` with exact-layout patterns, `_` and binds resolves statically on `Rec(L)` and branches on the layout id for `Obj`; a non-exhaustive match fails to tier 0.
- **Why no deoptimization.** A native fails and tier 0 reruns the call, rather than deoptimizing mid-body, which keeps every native side-effect-free until it returns.
- **Test.** `fuzz_typed_record_natives_agree_with_tier_zero` in `crates/jit/tests/test_tiers.rs`.

## Typed ry has its own numbers, `Bool`, `Str` and `List`

**Status:** built, except the closing note that typed `I64` code runs in the interpreter, which "Typed `I64` runs in registers, and list arithmetic fuses into one loop" supersedes.

- **Shape.** Two number types, `I64` and `F64`, plus `Bool`, `Str` and `List<T>`, all `NA`-free, so a typed program needs no R data type. Design `development/design/native-types.md`; contract the reference section "Numbers, `Bool` and `Str`".
- **Only `I64` and `F64`.** All thirteen Roc number types were tried: each width multiplied checker constraint cases, interpreter arithmetic and JIT register types, and no typed program used another. A width returns only when a benchmark program needs it. Any other literal suffix is a syntax error. `Constraint::Int`/`Constraint::Frac` default to `I64`/`F64` and have no spelling; `number` is the only written constraint. An R `integer` or `raw` becomes `I64` at the boundary.
- **Representation.** `F64`, `Bool`, `Str` reuse R's `double`, `logical`, `character` storage and cross into R for free. `I64` is its own vector type, `SexpType::I64` (code 28) with `REALSXP`'s layout; a scalar is a length-1 vector and `List<I64>` the same object at length n with the Array flag (`numbers::alloc`, `numbers::list`). No attributes, never ALTREP. A dense vector gives one representation for scalars and lists, a zero-copy handoff to `crates/typed`, no per-item tracing, vectorized list arithmetic (`numbers::arithmetic`, no recycling, overflow an error) and bit64 `integer64`-compatible storage. **A new type code beats a gp bit on `REALSXP`**: R and C code testing `TYPEOF` refuse it instead of reading the bits as doubles, and `is_atomic` is false, so every R builtin raises the `Num.to_r()` boundary error (`records::boundary_message`).
- **The checker decides a literal's run-time type.** A runtime with its own syntactic literal rules returned a `double` from a `match` arm adapted to a declared `I64` result, which broke soundness. Each `ItemCheck` records the unsuffixed literals it typed `I64` (`i64_literals`), `semantics::file_elaborations` maps them to ranges, and the preflight spells each as `5i64` (`preflight::elaborate`), the mirror of erasure for GNU R. Empty lists likewise: a library call whose result items come from no list argument (`check::builds_fresh_items`; `List.map` qualifies, `List.keep_if` does not) and `[]` are wrapped as `.ry_items(expr, "I64")`, a no-op on every other value, so a shadowing project function is safe. `List.keep_if` and the other slices need no wrapper because `standard_library::subset` gives an empty result its input's storage, and `numbers::operands` treats an empty list of any storage as an empty `I64` operand. Accepted costs: the spelling shows where code reads itself as data (`match.call`, `body`, `substitute`), and columns after an elaborated literal shift in run-time error messages.
- **Elaboration plumbing.** `ry run` hands the elaborated text to the runtime through `ry_set_script_source` and the console filter releases it as the command; it rewrites the file's own bytes (invalid bytes read as `?` so offsets stay byte offsets, then escaped as reading the file does), so a Latin-1 script keeps its bytes. A whole literal spelled with a fraction or exponent converts digit by digit, because an `f64` rounds past 2^53. `lower::with_annotated_literals` remains for `ry load` and `source()`, which run unelaborated text. A `List<I64>` given to `DataFrame()` becomes an `integer` column when every item fits, else `double`, as `Num.to_r` converts.
- **String and logical literal variables.** `greeting <- "hi"` binds a literal variable (`Constraint::StrLiteral`, `BoolLiteral`) that an annotated binding or typed parameter in the same item decides as `Str`/`Bool`, and any other read defaults to R's type, so plain R types as before. Meeting a `List` item variable binds the ry type directly in `unify`'s var-var arm (`infer.rs`), because no constraint spells "only `Str`".
- **Top-level literals in typed files are ry values.** In a strict file or one with ry syntax (`semantics::file_holds_ry_values`), a literal bound outside any function (`Checker::binds_ry_value`) defaults to `I64`/`F64`/`Str`/`Bool`. Letting later top-level items decide it was rejected because it breaks the per-item firewall (an edit to any reader would re-type the binding); keeping R types forced suffixes on every constant. A value of literals alone (`literal_value_scalar`) adapts like a lone literal. A number with `.` or an exponent and no `L`, or a whole number past `I64`'s range, defaults to `F64` (`NumberLiteral::default_native`), as Roc's `2.0` is a `Frac`. Plain R files keep R types. A host with no settings counts as strict for strict origins but not here, because this changes types.
- **The boundary is explicit.** An `I64`, or a `List` whose items share no R representation, passed to an R function is `NativeValueToR` naming `Num.to_r()`/`List.to_r()`, the static twin of the runtime error; `F64`/`Bool`/`Str` and lists of them pass unconverted.
- **Smaller rules.** `/` decides `F64` and is a `type-mismatch` on `I64`, as in Roc. Lengths, counts and positions are `I64` (`standard_library::whole_number`). List operators work item by item and never recycle. `try f(x)` is the `Ok` value or an early return of the `Err`; `try (expr)` keeps R's `try()`. No row variables: an open tag union cannot pass unlisted tags through with their types (user direction).

## Typed values are native, immutable, acyclic and freed by reference counts

**Status:** built, except the per-type struct-of-arrays pools with 32-bit references, which "Typed boxes have shapes, and a tag type is a union of them" supersedes.

- **Shape.** Typed ry's own values (numbers, `Str`, `List`, `Dict`, `Set`, records, tuples, tags, closures) live outside the R heap. They are immutable and acyclic (a closure never captures itself; recursion goes through a lifted definition), so compiler-inserted Perceus reference counts free them deterministically, with Lean 4-style borrow inference, field moves, drop specialization and reuse of a uniquely held block. No tracing collector sees them, so compiled frames need no stack maps.
- **R boundary.** R values stay under the collector and meet typed code only at the boundary: an entry stub checks a guess and runs a second compiled copy that reads the R value in place, or the program converts explicitly (`List.from_r`). References go one way: a native block may preserve an R value, but every native value crossing to R is converted (a typed closure becomes an R closure over converted captures), so no cycle spans both heaps.
- **Why.** Exact counts make in-place update a count-of-one test, freeing deterministic and leaks checkable at the end of every test. Native layouts are the Rust-shaped data the speed bar needs; R vector headers, intern tables and collector tracing would cost on every operation.
- **Cost.** A `List<F64>` reaches C only through a copy; an ALTREP wrapper is the deferred fix. Design and test matrix: `development/design/typed-runtime.md`.

## Typed code compiles ahead of time, and `ry build --aot --exe` writes an executable

**Status:** built. `ry run --aot` compiles a script's typed functions before it starts (the entry "`ry run --aot` compiles a script's typed code before it starts"), `ry install` writes an installed package's typed code file (the entry "The typed code file is position-independent code whose offset table is the per-process table"), `ry build --aot --exe` writes an executable (the entry "`ry build --aot --exe` appends the script and its code file to a copy of `ry`"), and plain `ry build` writes a source tarball.

- **Command line (user directive).** `--aot` asks for ahead-of-time compilation: `ry run --aot main.R` compiles before the program starts; `ry build --aot` and `ry install` compile into the installed package; `ry build --aot --exe main.R` writes one executable. Without `--aot` typed code compiles at its first call. Plain `ry build` writes a source tarball, as `devtools::build()` does, because existing workflows rely on it.
- **No code cache** (`AGENTS.md`, "Performance principles"). An installed package's code file is build output: one position-independent file per namespace, mapped read-only and shared between processes, stamped with the ry build and the package database, with every resolved name checked again when a function attaches. Code for a script or a package under development compiles in memory.
- **Tests.** Every typed program runs interpreted, compiled, and compiled with annotations erased, with identical output, plus an empty-pool leak check (from Bun's compiler, `prior-art.md`).

## `ry run --aot` compiles a script's typed code before it starts

**Status:** built for a script's top-level definitions and for files it sources into the global environment. The package code file is its own entry ("The typed code file is position-independent code whose offset table is the per-process table"). Not built: the executable. Decided against: packages under `ry load` (below).

- **Shape.** `ry run --aot` sets `RUN_FLAG_AHEAD`; `run_script` lowers every top-level expression before the first runs, creates the closure of each top-level `name <- function` (`crates/interp/src/ahead.rs`) and hands them to `Tier.ahead`. The typed core (`crates/typed/src/ahead.rs`) compiles each definition whose declared parameter types R can pass (numbers, `Bool`, `Str`, lists of those, tag unions, tables) plus its typed callees. Parameter types are read from the annotation text (`annotated_type`), which the preflight writes in canonical spelling, so the text is the checker's type.
- **Sibling calls.** A name not bound yet resolves to the closure created up front (`DEFINED_LATER`); the compiled call carries the usual function-identity speculation (`still_resolves`), and the assignment hands out that same closure (`ahead::take`), so the check holds once the definition runs. A different later binding fails the check and the caller recompiles.
- **Conversions have the checker's type.** The preflight wraps a `List.from_r(x)` whose item type the checker knows as `.ry_items(List.from_r(x), "F64")`, and the lowering takes that as the conversion's type (`Conversion.checked`) instead of an interpreter observation, which was a second source of truth and cost one interpreted call. One function (`arrays::checked_conversion`, `crates/interp/src/arrays.rs`) converts in both tiers (`do_items` in the interpreter, `write_converted` in compiled code). It widens as R coerces (`Bool`/`I64` items to `F64`, `Bool` to `I64`, an empty vector to an empty checked list), because the checker's R types are coarser than the vectors R computes (`abs(-3:3)` and `sample(5)` are typed `double` but hold integers). Any other difference is one error in both tiers. Rejected: erroring on every storage difference (it stopped correct programs), and recompiling for the arriving type (a second path used only by mistyped programs).
- **Lazy bindings** of a loaded namespace are forced when typed code compiles, in every mode: forcing reads the package database and runs no user code. An unloaded namespace waits for the first call, because loading it runs `.onLoad` before the script's first statement and changes output, options and error order.
- **Refusals are retried**: a refusal met before the script ran may only mean the script had not run yet, so refusals are dropped after the pass.
- **What still compiles at a first call**: a `Num.from_r`/`List.from_r` the checker cannot type, a call into a namespace not loaded yet (loading it early would run `.onLoad` first; waiting costs one interpreted call per caller), a function defined in a block or in a file sourced into another environment, a definition in a statement whose lowering warns, and a top-level function with a record, tuple, `Dict`, `Set` or function parameter that no compiled-ahead function calls.
- **Sourced files.** `parse()` of a whole file under `--aot` (`ahead::parsed_file`) compiles the file's typed top-level definitions only when the frame calling `parse()` binds `envir` itself to the global environment, read without forcing (`eval::peek_frame_variable`, which does not look in enclosing environments): `source()` at top level. `sys.source()` calls R-level `parse()` and compiles at first calls. Rejected: compiling every parsed file (closures nobody binds) and scanning the script for `source("...")` (misses computed paths, reads files before the script creates them).
- **`ry load` is not compiled ahead**: it defines functions lazily on first use, which is its point, and the compile already follows the definition immediately. Install the package to get its code file.
- **The pass must not change behaviour.** An expression whose lowering warns (`1.5L`) is left to the run (`ahead::holds_back_warning`), because the warning would precede a handler or `options(warn = 2)` the script sets earlier. Functions compiled ahead do not count against `MAX_COMPILED` (`ahead::compiled_ahead`), since a script's definitions are bounded by its text. The run roots each pre-lowered expression in its own handle scope, because nothing else holds a statement while it runs (`ahead::lowered`, pinned by `run-aot.scenario`).
- **Measured.** The up-front compile takes 1.1 to 5.9 ms per kernel or application; a first measured run drops by the compile it no longer holds (`pidigits` 51 to 2 ms, `log_rank` 34.56 to 22.02 ms); warm and whole-process times are unchanged.
- **Tests.** `typed_scripts_agree_however_they_compile` (interpreted, first call, ahead, ahead with every sibling guess failing under `RY_AHEAD_MISS=1`, tier 1 forced, annotations erased), `typed_benchmarks_run_natively` and `a_script_with_more_definitions_than_the_closure_limit_compiles_them_all_ahead` in `crates/ry/tests/test_scenarios.rs`; `run-aot.scenario`.

## An installed typed package holds elaborated sources with canonical annotations

**Status:** built.

- **Elaborated install.** `preflight::elaborate_package` checks a typed package (strict in `ry.toml`, or a file with ry syntax or `# typing: strict`), refuses the build on a strict error, and returns each file's elaborated text in the package's declared encoding. A rewrite that changes the line count or the parse errors fails the build. `build_tarball` (`crates/ry/src/main.rs`) builds a copy in a temporary directory (skipping `.git` and `.Rbuildignore` matches, entering each directory once so a link cannot loop), so the package directory is never touched. Without this, installed code had no checked conversion types and ran literals as R doubles where the checker said `I64`.
- **Canonical annotations.** Every file the preflight elaborates gets each one-line parameter and return annotation rewritten to the checker's type (`semantics::canonical_annotation`): aliases expanded, project nominal types unfolded except a recursive one inside itself, printed by `TypeRenderer`. A spelling is used only when reading it back gives the same type, so a rewrite never changes what was declared; unfolding stops at `UNFOLD_BUDGET`, because definitions naming another twice double the spelling each step. Open records, type parameters, unknown names and non-ASCII field names stay as written. `annotated_type` reads this closed grammar, so the lowering no longer needs its own reader of type definitions (one source of truth for named types). Elaboration keeps line counts, so srcrefs hold. Pinned by `benchmark_signatures_all_have_canonical_spellings` in `crates/typed/tests/test_annotations.rs`.
- **Serialization.** `I64` and `List<I64>` serialize as ry's type code 28 (array flag, length, each item's high and low halves; `crates/libr/src/serialize.rs`, `crates/interp/src/serialize.rs`), so a package's `I64` constants reach the lazy-load database through `lazyLoadDBinsertValue`; `saveRDS` and other builtins still refuse a top-level one. Records, tuples, tags, dicts and sets have no stream form and are refused.
- **Behaviour.** A parameter typed by a record alias is checked at run time as the record, where an alias reads as unchecked otherwise.

## The typed code file is position-independent code whose offset table is the per-process table

**Status:** built for installed packages and for executables. `development/typed-code-file.md` is the contract.

- **Why.** Session-compiled typed code embeds process addresses as immediates, so every process (each CI test process, per dependency) compiles and holds the same code privately.
- **Shape.** One file per namespace, `R/<package>.<build>.ryt`, written by `ry install` and `ry build --aot` in the session that writes the bytecode image (before `ryCodeImageWrite` in `code-image.R`, because writing the image ends the recording of the closures the file names). Every process value (helpers, libcalls, the interrupt flag, shape ids, static boxes, the file's own read-only data, R call sites, cross-namespace callees) is a word in a table at a fixed distance before the code, reached through the GOT load Cranelift emits for an external name on a PIC ISA; calls inside the file are direct. The writer resolves every relocation, so code pages are never written and are shared; only the table is private. Slots are numbered downward from the code, so the table needs no size before layout. Shape ids, which lowering writes as plain integers in a `match` and the sort comparator, get a shape constant in the IR.
- **Attach.** A closure is named by its lazy-load record offset and position, and attaches at its first call: names resolved at install are resolved again and must be of the class lowering specialized on, R call sites are built, callees attached, constructed shapes confirmed, and the session's `Compiled` entry inserted. A version that does not attach compiles in the session. A version whose direct callee is still attaching misses, so mutual recursion compiles in the session.
- **A version belongs to one closure.** The loader notes (record, position, database stamp) per closure when the database yields it; a run-time copy (R6 method, `environment<-`, per-class S4 method) has another environment, which a version's R call sites hold, so it compiles in the session. The note's stamp, not the disk database, is checked against the header, so a reinstall in a running session cannot attach new code to old closures.
- **Rejected.** Patching addresses into code pages at load (a patched page is private). A table pointer in a pinned register (costs every function a register and every entry a write). One emission mode for session and file code (session code memory is not at a fixed distance from a table); the targets differ in one `Slot` seam of the backend, and the tier matrix compares them.
- **Training.** With `--train`, every successful `COMPILED` entry of a database closure is also a root, except one whose guess a later conversion contradicted. A guessed conversion is stored with empty `checked`; at attach, `numbers::assume_conversion` records it as the session's observation, so a session that saw another type refuses the version and `conversion_epoch` makes it stale later. Pinned by `install-typed-package.scenario`. Untyped R is never in the file.
- **No cache.** The file is build output, replaced only by reinstalling; a stamp mismatch sends the namespace to session compiles with a line saying to reinstall. The file is trusted like a package's shared objects; hashes and stamps catch truncation, another build and a changed database.
- **Format choices.** Metadata is LEB128 varints in one hashed stream, because the loader parses it once into vectors, so a fixed-width on-disk layout buys nothing. Static strings live after the code; free stubs and data form one region hashed at map time, and each version's bytes are hashed at attach.
- **Platforms.** Linux maps the file; other Unix, and Linux where mapping is refused, copies into the same reserved range and flushes the instruction cache (`RY_TYPED_FILE_COPY=1` forces it). Windows does not use the file. The copy path compiles for `aarch64-apple-darwin` but has not run on macOS.
- **`ry run --aot` and packages.** A package is compiled at load only when its `R/` holds a `.ryt` of another build, because a package with no file cannot be told from one installed with `--ry-no-typed-code` without reading every function (`interp::ahead::namespace_loaded`).

## `ry build --aot --exe` appends the script and its code file to a copy of `ry`

**Status:** built. `reference/cli.md` ("Executables") documents the command and `development/typed-code-file.md` ("Executables") the layout.

- **Shape.** `[ry][zero padding to 64 KiB][typed code file][metadata][48-byte trailer]` (`crates/ry/src/executable.rs`). `main` reads its own trailer before parsing arguments (and before the R front ends, since an executable may be named `Rscript`), so every argument belongs to the script. A damaged payload is an error; an unreadable executable (execute-only, no `/proc`) is plain `ry`. The run skips the checker and takes `ry run --aot`'s path, attaching first calls through `typed::file::open`.
- **Script identity.** A script closure's note is `(SCRIPT_DATABASE = u32::MAX, top-level expression index, 0)`, stamped with the elaborated text's length and hash, so a closure whose index or text differs compiles as under `ry run --aot`.
- **Sourced files are read from disk**, because embedding them needs a virtual file system under `source()`.
- **A development build records its runtime paths** (runtime library and base R), because a copy moved out of `target/` cannot find them; they go to `repl::use_runtime_files`, not `RY_LIBR`/`RY_BASE_R`, so child processes (maybe another ry build) do not inherit them, and those variables still win.
- **The build protects its inputs**: it refuses an output that is the script itself (an extensionless `main`) or a directory, makes the copy owner-writable (an installed `ry` is often 0555), and removes a partial file on failure. `RY_AHEAD_MISS=1` forces first-call compiles, which the scenario uses to check the fallback.

## The typed core lowers R syntax at the first call and runs on 16-byte strings and view lists

**Status:** built.

- **Shape.** `crates/typed` lowers a strict typed closure to a typed SSA IR at its first call for the argument types it sees, inserts Perceus reference counts (`ownership.rs`) and compiles with Cranelift (`backend.rs`). `jit::native::call` is the one seam; `typed::call` returns `None` to fall back. A run-time failure (an `NA` an integer cannot hold, overflow, index out of range, interrupt) returns status 1 and the interpreter reruns the call, so the typed core never reports an error itself.
- **Lowering from R syntax, not salsa**, because the runtime cannot link the analysis and the closure body is the source of truth the interpreter also runs. `Speculation` guards on resolved functions, rechecked every call, make it sound against rebinding.
- **`Str` is 16 bytes, not 24**: two words fit Cranelift's tail-convention two-register return and halve list storage; up to 15 bytes are inline, flagged by bit 63 of the second word.
- **A `List` header points at its items**, so an R vector argument is a view (flag `VIEW`) over R's data with no copy, and one compiled body serves owned and borrowed lists. Writing to a view copies first.
- **Base closures behind lazy-load promises.** A function is base's own when its binding cell is base's, and `resolve_function` forces the promise (it only reads the package database) instead of refusing.
- **Counted loops check their ends once.** `for` over `seq_len`, `seq_along`, `a:b` or a vector tests `counter == end` at the latch and steps with `Op::Step`, which cannot overflow because the counter never passes the end.

## Typed `I64` runs in registers, and list arithmetic fuses into one loop

**Status:** built.

- **`I64` is a register value** with checked `+ - *` (overflow fails) and R's floored `%%`/`%/%`; a failure reruns in the interpreter, which reports the error.
- **An `I64` parameter or result converts an exactly whole R number** (`annotations::check_value`, `check_return`, `check_forced`), mirrored in `typed::call`, inlined calls and returns; otherwise `f(3)` failed and every caller wrote `3i64`. A `List<I64>` boundary converts whole double or integer lists item by item (`annotations::as_i64`), because an unsuffixed `[1, 2]` is a double list at run time, and an empty `logical` list becomes an empty `I64` list (`annotations::lacks_item_type`), covering unelaborated `ry load`/`source()` code.
- **`List<T>` annotations are checked at run time**: the Array bit plus the item's storage (an R list for records, tags, functions and nested lists); any empty list passes, because an empty list is stored as `logical`.
- **Fused elementwise kernels** (`Op::Elementwise`, `values::ry_elementwise`). A tree of base arithmetic over same-length lists and scalars, with at least two list operations or one under `List.sum`, lowers to one program of up to 16 steps over up to 16 inputs, run in 128-item chunks so intermediates stay in L1 and only the final list is allocated. Each intermediate is checked for NaN, so a fused tree fails exactly where the unfused one would.
- **Large blocks get power-of-two pools.** Blocks over 64 KiB round up to a power of two, and up to four freed blocks per class (each at most 64 MiB) are kept per thread (`values::block_alloc`), because the system allocator mapped fresh pages each call and paid page faults.
- **Library constants are speculated by identity.** `Num.pi` is a constant when the name resolves to the very object `standard_library::builtins_env()` holds (`lower::resolve_value`, `typed::still_resolves`); a function that assigns the name anywhere refuses (`lower::assigns`), since a read before a later assignment in a loop would keep the constant.
- **`Num.pow` is `powf`, not R's `^`**: R's `r_pow` multiplies out small whole powers, which changes the last bit (`Arith::NumPow`).
- **A counted `List.range` loop keeps the list length limit** (`i32::MAX` items), though it allocates no list.
- **A constant scalar default is passed as the argument** (`jit::native::call`), so a missing argument with a default does not refuse the call.

## Array subscripts walk offsets instead of listing them

**Status:** built.

- **Shape.** `ArrayOffsets` in `crates/interp/src/subset.rs` yields `x[i, j, k]` offsets one at a time to `select_positions`, `scatter` and `recycle_into`, instead of materializing a column-major offset list (an 8 MB list per `X[, , k]` read of a 1000 x 1000 x 20 array). The innermost run walks the first selection longer than one position; an odometer step updates the base by each changed dimension and counts `NA` positions, amortized constant time.
- **Trap.** `next` is inlined and the odometer step is out of line, which keeps the iterator in registers in the gather loop; without those attributes the row read ran about 50% slower than with listed offsets.
- **Tests.** `NA`, empty and zero-length shapes: `crates/oracle/cases/assignment__array_slab_shapes.R`.

## Typed boxes have shapes, and a tag type is a union of them

**Status:** built, except the deep conversion of a box result, which "A typed record or tag that R holds stays a box until R reads into it" supersedes.

- **Boxes.** A record, tuple or tag is a counted allocation (`values::Boxed`): a header with the count, the shape id and two bitmasks (words holding counted pointers, first words of strings), then one word per field and two per string. Freeing reads only the header, never the shape table. A field-less shape (`:B`) is one static, uncounted box per shape (`shapes::constant`), so code bakes its pointer in.
- **Shapes** (`crates/typed/src/shapes.rs`): a heap layout (the interpreter's `heap::records::Layout`) plus each field's item type, so code knowing a box's shape reads fields at fixed offsets. `Type::Record(shape)` names one shape, `Type::Tag(union)` an interned union. A tag-typed *field* is normalized to any tag (`shapes::ANY`), which keeps recursive types finite.
- **Matching any tag.** A tag from R has type `Tag(ANY)`, because the runtime does not check tag-union annotations. A `match` over it switches on the shape id (`Op::ShapeOf`) over the *confirmed* shapes a pattern names and gives up on any other. A shape is confirmed when the boundary converts it or when code constructing it compiles (`Lowered::constructed`, confirmed only once the whole function lowers, so a refused lowering confirms nothing). Confirming can grow the set a `match` in the same function relied on, so `compile` lowers again until its epoch is current, and `typed::compiled_for` recompiles when the set grows. Correctness rests on the switch being exhaustive over what compiled code can see, which the epoch guarantees.
- **Stray shapes from ill-typed R.** When an arm does not lower for one confirmed shape, `Lowering::rewind` drops it and that shape alone gives up. A `match` whose value is the function's result (`lower::tail_matches`) lowers each arm against the declared result type (`typed::declared_result`, `switch_shapes`'s `target`), so an arm whose value cannot have the type is rewound; a `match` elsewhere has no declared type and refuses when its arms disagree.
- **Ry lists in a tag from R** (`typed::list_of_array`) convert deeply into owned native lists, taking the item type from the value; compiled code that meets a field type it cannot use gives up. An empty logical `Array` (a bare `[]`) cannot say its type, so it refuses rather than confirm a `List<Bool>` shape. Every boundary refusal carries its reason into the `RY_TYPED_LOG` line.
- **Boundary.** A record or tag argument from R converts deeply into boxes (`typed::to_box`). Plain record and tuple *parameters* are not taken from R (the typed-record-parameter design is parked); records pass natively only between typed closures.
- **Alias-only annotations.** `annotations::register_definition` skips only a closure with no annotation text, because one annotated only with aliases or tag unions has empty storage masks yet is typed.
- **Typed calls are never read ahead.** `native::pure_value` does not evaluate a typed call inside an unforced argument promise: the call may not finish, and R never runs an argument its callee does not force (`ignore(1, spin())` must return).
- **Test.** `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` in `crates/jit/tests/test_tiers.rs`.

## Typed-to-typed calls are eager on every tier

**Status:** built.

- **Rule.** Typed ry is strict: a call from one typed function to another computes every argument the caller wrote, left to right, before the callee runs, on every tier. Otherwise compiled code (`Op::CallTyped`, eager) and tier 0 (lazy promises) disagreed on an argument that never finishes or has an effect.
- **Mechanism.** `interp::eval::force_typed_arguments` forces, before a typed callee is entered, each argument promise whose environment is the frame of a live typed closure; a typed closure never runs on slots, so the environment test is exact. A promise written in R code, including one forwarded through `...`, stays lazy, so calls from R keep R's semantics and `native::pure_value` never reads a typed call ahead.
- **Defaults.** `native::gather` also passes a default that names no formal and that `pure_value` reads in the closure's environment (`dt = 1f64 / 12f64`).
- This argument order is the one documented difference from GNU R running the erasure (`type-system.mdx`, "Annotations at run time"). Pinned by `test_tiers::typed_to_typed_calls_evaluate_their_arguments_first`, JIT off and on.

## R lists do not convert to records at a typed boundary

**Status:** built, except the "Gap" bullet, which "A ry list is one item word" supersedes.

- **Decision.** R values enter typed code only through the per-parameter boundary, as plain vectors and scalars checked at entry; records, tuples and tags are built inside typed code. A plain R list where a record is expected is a `type-mismatch` (`reference/type-system.mdx`, "Records from R"); a record from `as.record()` passes the check but runs in the interpreter, which `RY_TYPED_LOG` names.
- **Rejected (by the owner): record parameters filled from R**, with fields spread into separate compiled parameters. An R list is untyped and mutable, so the boundary would be implicit (field types, lengths and `NA`s checked deep inside a call instead of where the caller converted), it adds a second conversion route beside `List.from_r()` and `Num.from_r()`, and spreading fields gives the typed core a second kind of binding with its own refusals.

## A typed call that runs in the interpreter always says why

**Status:** built.

- **Decision.** Every point where the native path declines calls `annotations::decline(|| reason)`, and the fallback prints ``typed: `name` runs in the interpreter: <reason>`` once per name and reason under `RY_TYPED_LOG`, the typed core's explain path. The closure runs only under the log, so a decline costs one flag test. The compile table keeps each refusal's text, so a cached refusal still gives its reason. Dispatched methods and sessions without a native tier log from `eval::try_native_typed`. ``the native path recorded no reason, which is a bug`` marks a missed decline point.
- **Unforced promises are reported, not forced.** Forcing an R-written argument at the boundary would run its code before the body and run arguments the function never uses (`reference/type-system.mdx`, "Parameters are checked when their value is computed"). The native tier reads ahead only what cannot run R code (`native::pure_value`); anything else declines with a reason that quotes the argument and says to compute it into a variable first.
- **Pinned by** `typed-fallbacks-give-a-reason.scenario` and `test_scenarios::typed_benchmarks_run_natively`, whose failure message is the fallback lines.

## Typed code calls R natively

**Status:** built.

- **Decision.** A call of an untyped R function (base, an R closure, or `pkg::name` of a loaded namespace) compiles to `Op::CallR`, calling `typed::ry_call_r` with a leaked, globally rooted `lower::RCall` site, in exactly two positions: a statement whose value is unused, and the direct argument of `Num.from_r`, `Str.from_r`, `Bool.from_r` or `List.from_r`. Anywhere else the value would be an untyped R value, which typed code cannot hold.
- **Interpreter semantics.** Each argument reaches the function as a forced promise of the code written, so `substitute()`, `sys.call()` and error calls match. `Num.to_r`/`List.to_r` arguments and the result conversion run the real standard library functions through `eval::apply`, so errors are the interpreter's. An `I64` crosses only through `Num.to_r` or `List.to_r`; a set or dict never crosses. An R call inside another R call's arguments is refused, because R's lazy arguments decide that order and compiled code computes arguments first.
- **Result types come from feedback.** `Num.from_r` and `List.from_r` give a type that depends on the R value, so the interpreter records it per call (`numbers::observed_conversion`). Without a record, lowering refuses with `lower::AWAITING` and `typed::AWAITING` remembers what it waits on, so the first call runs interpreted and later ones natively. An unloaded `pkg::name` waits the same way, because resolving it at compile time would load the namespace. A conversion the checker typed (`Conversion::checked`) gives its type or an error; others are guesses.
- **A failed guess reruns the call in the interpreter.** `numbers::Observed` is `Steady(type)` or `Changed(first, second)` per call; a change bumps `numbers::conversion_epoch`. Native code sets `typed::FAILED_GUESS` and gives up; code that guessed holds the epoch (`Compiled::guessed`) and goes stale; lowering refuses a `Changed` conversion. So every tier prints what the interpreter prints. Rejected: an R error for the changed type, which the interpreter never raises.
- **Giving up after an R call replays it** (`interp::replay`). Rerunning from the start would repeat the R call's effects. Each `typed::call` opens a segment; `ry_call_r` records `(call node, value)` for every R call that is not `effect_free` or that warned, and `replay::trace` roots the values. On a give-up, `replay::replay` runs the call in the interpreter with the records pending; `run_frame_general` then evaluates a typed closure's body through `replay::body`, wrapping each pending node as `.ry_replay(rewritten, original)`, which returns the first unused record for the node (marking nested calls' records used) or evaluates live. Native `ry_call_r` consumes records the same way, so nested give-ups replay too. Typed code computes only from its arguments and R calls, so the interpreter reaches the recorded calls in the same order.
  - Rejected: rerunning the call whole (repeats effects), hooking every interpreter call path (too many), running a copied closure (loses the signature, keyed by formals and body).
  - Limits: `replay::MAX_RECORDED` calls and `replay::MAX_RECORDED_MIB` of values per segment, because records keep values alive; past them a give-up is an error that says so.
  - File-attached code must hold the body's own call nodes (`file::Place`), because replay and `numbers::assume_conversion` key on the node.
  - An error an R function raised (parked in `PARKED`) is raised as is; `stop(...)` is a site marked `diverges` followed by a give-up, and does not set `calls_r`.
- **R calls run in a frame for the typed call.** `ry_call_r` pushes one (`eval::run_in_frame`) from the `TypedCall` in `typed::RUNNING`, with a fresh environment enclosed by the closure's, so `stop()`, `sys.call()` and `parent.frame()` see the typed call. Only R calls pay the environment.
- **Lazy arguments.** If a read-ahead argument (`native::pure_value`) depends on a variable, an R call in the body could change it before R would have forced it, so code that calls R (`Compiled::calls_r`, including callees) declines those calls with a reason.
- **Pinned by** `typed-code-calls-r.scenario` and `test_scenarios::typed_benchmarks_run_natively`.

## Typed list literals compile natively, and an empty list of boxes settles its item type at first use

**Status:** built.

- **Shape.** `[a, b, ...]` lowers to an empty vector of the items' common type plus one `Op::Push` per item (`list_literal` in `crates/typed/src/lower.rs`); mixed item types refuse. `List.append` is `Op::Push`. `.ry_items([], "I64" | "F64" | "Str")` gives the named type, and a bare `[]` is a `Bool` list, because the preflight wraps every other empty list.
- **Empty lists of records or tags** are named only by storage (`"list"`), so such a `[]` starts as a *pending* list of any tag; the first `List.append` or assignment meeting a concrete list settles it, retyping every holder and the allocation (`Lowering::settle`). Settling refuses when anything but a copy or a length read used the placeholder (that operation chose code by the placeholder type), and reading a pending list's items refuses. Never settled, it stays an empty list of any tag, which is sound.
- **Rejected: carrying the checker's type** through the preflight: it needs a type printer and parser for records, tuples and tag unions in the runtime, a second description of the same type.
- **Pinned by** `typed-list-literals.scenario`.

## A count of a logical list fuses with the comparisons that built it, after lowering

**Status:** built.

- **Shape.** `count_if` with the identity predicate over a `List<Bool>` lowers to `Op::CountTrue`. `crates/typed/src/fusion.rs`, before ownership insertion, follows the list back through `VectorCompare` (`I64`/`F64`), `VectorLogic`, `VectorNot` and same-typed `Copy` and makes one program over the comparisons' inputs. `values::ry_count_true` runs it in 128-item chunks with lanes 0, 1, 2 for `FALSE`, `NA`, `TRUE` (`&` is `min`, `|` is `max`, `!` is `2 - x`), and fails on `NA` in the result, a length mismatch or an invalid program, as the unfused code would.
- **Why after lowering.** At lowering an operand is a local that may be reassigned, read in a branch or returned. The IR pass fuses only within a straight-line run ending at the count (its block and the chain of single-predecessor blocks into it) and checks no input is reassigned between comparison and count.
- **Comparisons and `!` on ry lists give `List<Bool>`**, not R logicals, so `&` and `|` with another ry list accept them.
- **Pinned by** `typed-mask-counts.scenario` and `fused_counts_follow_three_valued_logic` in `crates/typed/tests/test_values.rs`.

## A uniquely owned Array grows in place

**Status:** built.

- **Decision.** The ownership rule for R lists (`owned`: refcount 0, or the reusable binding with refcount 1) applies to a non-empty Array, so `List.append`, `List.set`, `List.concat` and `List.swap` update in place with amortized constant time, as the standard-library page promises. An update checks the new item against the storage without allocating (`arrays::holds_item`), then stores (`arrays::store_item`) or appends into spare capacity, doubling when full (`append_owned`). A misfit item takes the copying path, which reports the error and never sees a half-updated Array. An empty Array is `logical(0)` until its first item decides storage. The moves journal (`crates/interp/src/moves.rs`) records replaced items so a failing helper restores the original.
- **Rejected: a separate List representation.** An Array is already the one storage of a ry list in the interpreter and typed code; the problem was ownership.
- **Reserved room.** `List.with_capacity`/`List.reserve` of an empty Array give an empty resizable `logical` Array whose capacity is the room; the first append keeps it, and `.ry_items` keeps it when retyping. Typed code lowers `.ry_items(List.with_capacity(n), "I64")` (written by the preflight, since only the checker knows the item type) to `Op::EmptyList`.
- **Pinned by** the Array growth and `move_undo` cases in `crates/interp/tests/test_standard_library.rs`.

## A ry list is one item word

**Status:** built.

- **Shape.** `Item::Array(id)` is a ry list held as one counted pointer word; `id` interns the item type in `crates/typed/src/shapes.rs` (`array_of`, `array_item`), so `Item` stays `Copy` and one word at any nesting. Stores use `Type::held_item`; `Type::item` is unchanged because comparison, `rep` and vector conversion must keep refusing a list of lists. In R, a list of ry lists is an R list of Arrays.
- **`Bool` joins logical.** A `Bool` is a never-`NA` logical in the same word, so a branch or `return` mixing them joins as logical (`joined`, `exit` in `lower.rs`).
- **An empty `I64` sum is 0** in `ry_sum_i64` and `ry_elementwise`, as in the standard library.
- **A copying push grows from the length**, not the shared list's capacity, which otherwise doubled on every copy in a loop of field updates.
- **Pinned by** `typed-record-list-fields.scenario` and the list-field cases of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` (`crates/jit/tests/test_tiers.rs`), which also check for leaks.

## Typed `List.sort_with` is a merge sort with its comparator inlined, and strings order by the interpreter's collation

**Status:** built.

- **Sorting.** `sort_with` in `lower.rs` builds a bottom-up merge sort in the IR with two buffers that trade places per pass, and inlines the comparator once into the merge step. A right item goes first only on `:GT` (`Op::ShapeOf`), which keeps the sort stable as the standard-library page promises. Rejected: a native sort that calls back, which needs a compiled comparator with a native entry, which inlined lambdas lack.
- **Tuple index.** `[[` on a tuple with a constant position is `Op::Field`; a computed position, or any index of a named record, refuses, because the field's type depends on the position.
- **String order.** `values::str_order` calls `interp::sort::collate`, the function behind R's `<`, for `ry_str_compare` and list comparisons. The interpreter reruns failed native calls, so the tiers must agree in every locale; one function is the only way, and `collate` reads the locale per call, so `Sys.setlocale` is honored.
- **Pinned by** `typed-sort-with.scenario`, `test_tiers` sort and string cases, and `comparisons_and_logic_follow_r` in `crates/typed/tests/test_values.rs`.

## Converting a record tree at the typed boundary takes one shape-table borrow and no per-node allocation

**Status:** built.

- **Shape.** `to_box` finds shapes through `by_layout`, a dense vector indexed by layout id (ids come from an append-only interner), scanning that layout's few shapes with no hashing or allocation. `shapes::with` lends the whole table to one conversion, and `Shapes::converted` interns, confirms and returns words, masks and static box in one call. Fields convert into stack buffers laid out as the box (offsets are prefix sums of `item_words`), so the box fills with one copy.
- **`from_box` borrows briefly**: it copies out the layout and items, because allocating an R record may collect and so run R code that reaches the table.
- **Stack** is checked every `STACK_CHECK_LEVELS` levels, so deep chains (200,000 levels) convert both ways without querying the stack per node.

## A typed record or tag that R holds stays a box until R reads into it

**Status:** built.

- **Shape.** `from_box` returns a lazy record: an R record of the right layout whose layout word carries `heap::object::LAZY_RECORD`, slot 0 holds the box, all slots marked raw (the collector traces nothing), refcount `REFCOUNT_MAX` (no in-place update writes to it). `interp::records::slot_value` and `with_slots` fill it one level on first read (children become lazy records), and every other slot reader goes through them; `identical()` fills through `settle_uncollected`, which holds off collection, and compares two lazy records over one box without filling. `to_box` of a lazy record takes another reference to its box. `Interp::lazy_records` owns one reference per lazy record and releases it at the collection that finds it dead or filled, safe because the heap never moves objects.
- **Compiled code** never reads into one: it fails the JIT's exact layout compare for `$` and the native field load (`jit::native::field` checks the layout word's sign), so the interpreter path fills it; `argument_word` does not pass one natively.
- **Native memory.** Boxes are malloc'd and invisible to the collector, so `from_box` runs a minor collection once the typed core's live object count passes a threshold (at least 2^20), then doubles the threshold past what survived.
- **Filling roots the record first**, because the reader may hold it only in a register and a collection mid-fill would release the box being read.
- **Not done:** a record R built itself still converts deeply per call, and a filled record keeps no link to its box.
- **Pinned by** `typed-lazy-records.scenario`, also under collection stress with the JIT forced on.

## The typed suite scores typed ry against Rust

**Status:** built.

- **Shape.** `tools/bench/typed/` is a suite like the others: `suite.toml` names each program's `typed_source` (owned by the kernels or applications suite) and `rust_source`, and every twin includes `harness.rs`, which prints the typed harness's line. `bench.py` runs it with Rust as baseline, needs `rustc` and no GNU R, and fails a program whose checksums differ. `just climb` times through the same runner, so one copy of the twins and one statistic exist.
- **A results file names its baseline and ratio direction**: `baseline` (`gnu_r`/`rust`) and `ratio_kind` (`speedup`/`slowdown`); missing keys read as `gnu_r`/`speedup`. A typed-over-Rust ratio is a time where lower is faster, so renderers keep it out of every speed-up table, chart and sentence.
- **`rustc -O` on one file**: level 3 like cargo release, no target-CPU flag, no cargo project per program; flags and version go to `meta.rust`.
- **Too-short floor per suite**: 0.1 ms on Rust (`RUST_SHORT`), stored as `meta.too_short_seconds`, because both harnesses time warm runs in-process.
- **The typed harness reads `Sys.time()`** with six decimals, because `proc.time()` is a millisecond clock.
- **No peak memory**: `wait4` reports a child's peak no lower than the runner's own at fork, which exceeds a Rust twin.
- **`--any-build`** lifts the release-build refusal for a local look; `--write` still requires `--release`.
- The bar for the suite's geometric mean is 2x Rust's time or better (user direction).
- **Pinned by** `TypedSuite` and `BuildProfile` in `tools/bench/test_bench.py`.

## Nativeness is a strict-mode diagnostic

**Status:** built.

- **Decision.** `semantics::native::native_blockers` judges each native function after its item is checked, from HIR and recorded types, into `ItemCheck.native_blockers`; `strict_diagnostics` renders them, and the errors reach the `preflight` gate. It is a pass over one item, not a query, so the per-item firewall holds; it reads other definitions only through declared signatures. Contract: `reference/type-system.mdx`, "Native functions".
- **Which functions are judged.** A function literal whose declared signature (inline or `#:`) is written entirely in ry types (every parameter one, result one, `NULL` or absent, at least one ry type). A signature with an R type is the opt-out boundary function, because it is what the author wrote and every caller sees, and such a function cannot avoid holding an R value. An unannotated literal is judged only inside a native function. Only a signature that is a type alias name gets an extra `interpreted` warning (`Uncompiled::AliasedSignature`). A type parameter bounded by an R type (`native::has_r_bound`) makes a boundary function, so `ry check --annotate` output on plain R is not judged as ry.
- **Two codes.** An R value in a native function is a `native` error (the author broke the rule and can fix it). Legitimate ry the typed core does not compile yet (`Uncompiled`: `try`, list patterns, a std function without native code) is an `interpreted` warning: as an error, strict mode refused about 195 std functions and `try`.
- **The rule.** Every value is a ry value. An R function may be called as the direct argument of a `*.from_r`, as an unused statement, and as `stop(...)`. Table values are ry values (`native::gives_table_value`, which must agree with `gives_table_value` in `crates/typed/src/lower.rs`).
- **Sound in one direction**: a native function with no finding compiles. The checker over-reports where it cannot decide (base functions the core computes itself, `a:b` in `for`). `native::COMPILED_STD` mirrors `Lowerer::builtin` because `semantics` cannot depend on `typed`. `Expr`-building operators are told from arithmetic by operand form on both sides (`Lowerer::is_table`, `Judge::table_operand`).
- **Each function is judged alone**: callers of a blocked function get no finding, and an R value is reported once, where it first appears.
- **Not covered**: calls the native entry declines (record, tuple, list literal or function arguments, non-constant non-variable arguments, `Dict`/`Set` results, first calls awaiting feedback) run interpreted without a finding.
- **A variable keeps one layout**, because the typed core gives it one machine type: `Judge::layout_differs` mirrors the lowerer's value classes; an `if` keeps a name only when both branches agree. A local function is walked where it is first called, since it reads the caller's variables as they are at the call.
- **Pinned by** `crates/semantics/tests/typing-strict/native.R.test` and `test_scenarios::native_diagnostics_agree_with_the_typed_core`, which fails when a function without a finding falls back or a `COMPILED_STD` name goes unexercised; its `NATIVE_CHECK_EXCEPTIONS` list only shrinks. Programs under `crates/ry/tests/native/` state their blocked and boundary functions in a header.

## Typed code holds table values natively

**Status:** built.

- **Shape.** `ir::Type::Table` is one opaque word, the pointer of a DataFrame, LazyFrame, GroupBy, LazyGroupBy or Expr (`frames::dataframe::is_table_value`). Typed code only passes it to the table API (`gives_table_value` in `lower.rs`), compiled as `Op::CallR` with `RResult::Table`. It is not counted: the collector owns it.
- **A table call is not an R call for the R-call rules**, because the table API changes nothing a second run would see: it may nest in arguments, does not set `Compiled::calls_r` and does not count in `R_CALLS`. Its one visible effect is a warning, so `ry_call_r` compares `interp::conditions::warnings_raised()` around it and counts the call when the number moved.
- **Operators** with a table operand are table calls of the base operator, decided from syntax (`Lowerer::is_table`, `lower::returns_table`). The runtime keeps no alias definitions, so a result type that aliases a table type is not seen; the checker warns and asks for a variable.
- **Columns are R vectors**: `table$name` and `table[["name"]]` compile to base `[[` and are legal only where an R call's value is; `table[...]` is refused with a message naming the verbs.
- **Rooting.** A table result sits in a word the collector does not scan, so `ry_call_r` roots it on the handle stack until the outermost `typed::call` returns; a loop making a table per iteration holds them all until then.
- **Pinned by** `typed-table-values.scenario`.

## A serialized typed closure carries its signature

**Status:** built.

- **Decision.** The registry (`interp::annotations`, keyed by formals carrying `TYPED_DEFINITION`) stays the one in-memory source of truth. The stream carries a copy: `libr::serialize` writes `annotations::STREAM_ATTRIBUTE` as the first attribute, and `interp::serialize` removes it and registers it (`annotations::restore_signature`), so a closure in memory never has it. Without it a typed function read back (lazy-load database, `readRDS`, saved workspace) kept the typed bit but had no checks or conversions. Another R reads such a stream as a closure with one unknown attribute.
- **Unloaded siblings.** `typed::lower::resolve_function` forces a lazy-load promise of any package (`serialize::lazy_record_header`), because such a promise only reads the package database.
- **Pinned by** `install-typed-package.scenario`.

## A record or tag that stays in its function has no box

**Status:** built.

- **Shape.** `typed::unbox` (`crates/typed/src/unbox.rs`) runs after lowering and before fusion and ownership. A local holding a record or tag becomes one `Int` local for the shape plus one local per field some instruction reads. Constructions assign the shape constant and copy read fields, a payload-less tag (`shapes::constant_shape`) assigns its shape, `Op::ShapeOf` and `Op::Field` read the variables. Every assignment of the old local assigns the new ones at the same place, so a read sees the same value on every path. Running before ownership means no count operation ever exists for a removed box.
- **Rejected: a dominance rule** (a single dominating construction). It treated locals as SSA values and missed boxes built on both arms of an `if` or rebuilt per loop iteration. Locals are variables, and a variable holding a box can simply become several variables.
- **Groups.** Locals copied into each other form a group (union-find over `Op::Copy`), replaced whole or not at all. A group keeps its boxes when a member is a parameter's box, is assigned by anything but a construction, in-group copy or static tag, or is used other than as a shape read, field read or in-group copy; or when reads of one field differ in type, or constructions store types the field type does not cover (`fits`).
- **Payload-less tag variables** are replaced too when some instruction builds for the group, so a `match` compares a register instead of loading from the static box.
- **A field only one shape has** may be dead-but-live after a join (liveness is not path-sensitive). Sound because locals start at zero, a zero string, list or box counts as nothing (`backend::dup`/`drop` test it), and a live local holds zero or a value it owns.
- **Nested boxes** go in later rounds (`MAX_ROUNDS`).
- **Pinned by** `crates/typed/tests/test_unbox.rs`, `typed-records-unboxed.scenario`, and the loop case of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text`.

## A small typed function is lowered into its caller

**Status:** built.

- **Shape.** `Lowering::inline` takes the callee's storage rule (`crate::storage`, so arguments meet the callee's annotation exactly) and serves top-level typed functions as well as local ones. A call inlines when the callee is in the same environment, is not already being inlined (`inlining`), is within `MAX_TYPED_INLINE_DEPTH` levels, takes exactly the arguments passed, cannot end invisibly (`tail_invisible`), and is small (`small_body`: at most `MAX_INLINE_CALLS` calls and operators, no `return`, no self call). Inlining lets the callee's constructions land where `typed::unbox` removes them.
- **Self-recursion inlines one level**, for a function with at most `MAX_SELF_CALLS` self calls: each further level or call multiplies the body, and compile time is part of the score (`expr_tree`'s five-way recursion compiled five times slower for no gain). `self_inlines` counts copies as made, because a local function holding the self call is lowered at each of its calls. A recursive function never inlines into another, so a caller's size does not depend on its callees' recursion.
- **Why at lowering, not on the IR.** The lowerer types by flow, so an inlined body is specialized to the site's argument types for free; an IR inliner would need the callee's IR for those types plus renaming.
- **Fallback.** An inlined body is lowered under the caller's rules, which can refuse; `lower::lower` then runs once more (`lower_calling`) with that callee as `Op::CallTyped`.
- `RY_TYPED_LOG` prints no "compiled for" line for a function that was only inlined.
- **Pinned by** `typed-record-list-fields.scenario`, `typed-calls-stay-native.scenario` and the self-calling cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic`.

## A typed loop over a held list counts a position and reads without a range check

**Status:** built.

- `positions` and `count_up` in `lower.rs` run an `I64` counter from 1 to a length read once; `next_position` leaves when the counter equals the end before stepping, so the step cannot overflow. The body reads with `Op::ItemAt`, an unchecked load, which is sound because the loop reads a lowerer-made local holding its own reference for the whole loop, so the length cannot change. `List.walk` over `List.range` builds no list.

## A typed free calls a register-preserving entry

**Status:** built.

- The backend compiles one thunk per free routine in Cranelift's preserve-all convention (`CallConv::Preserving` in `crates/ssa`) and calls that, so values live across a drop stay in registers. The spills came from the host call's convention (the allocator assumes it clobbers every caller-saved register), not from its frequency, so inlining only the count test did not help.

## Fused list kernels are compiled per CPU feature level and clear no buffers

**Status:** built.

- **Feature levels at run time.** A released binary must run on any x86-64, so not `-C target-cpu`. `count_true` is `#[inline(always)]` and compiled twice (`count_true_avx2` with `#[target_feature(enable = "avx2")]`, picked by `is_x86_feature_detected!`). Trap: its helpers (`operand`, `compare_lanes`) must be `#[inline(always)]` too, because a function without the attribute is not compiled with the caller's features. `crates/numeric/src/vector_math.rs` does the same.
- **The arithmetic kernel** calls one step function per step and chunk through a pointer chosen once (`Step<T>`: `step_dbl_avx2`/`step_dbl_baseline`, same for `I64`). The slices are step parameters, so the compiler knows they do not overlap and vectorizes every operator with its `NaN` or overflow test; the last step writes into the result list. A call per 128-item chunk costs nothing beside the chunk.
- **No clearing.** `Rows` holds chunk buffers uninitialized; a kernel fills its scalar rows, clears only rows its steps write, and reads no other row. Zeroed arrays cleared up to 36 KB per call.
- **Pinned by** `fused_counts_follow_three_valued_logic` and `fused_arithmetic_agrees_with_one_item_at_a_time` in `crates/typed/tests/test_values.rs`, and `typed-mask-counts.scenario`.

## `Dict.update` probes once and builds no `Result`

**Status:** built.

- The lowerer emits `Op::DictFind` (position or -1), branches, and lowers an inline `f` once per branch: found reads with `Op::DictAt`, applies `f` to `:Ok(value)` and stores with `Op::DictPut` at the known position; absent applies `f` to `:Err(:Missing)` and stores with `Op::DictInsert`. Each branch's `Result` is built and read in one function, so `typed::unbox` removes it. A called (not inlined) closure keeps one joined lookup and one call (`lower::dict_update`, `store_update`).
- **Why two copies of the callback**: one copy at a join sees a `Result` assigned on two paths, which unboxing keeps as a box. The cost is bounded by `MAX_INLINE_CALLS`.
- `ry_dict_put` falls back to an insert when the dict is shared, static or the position is -1, so `Op::DictPut` is correct wherever `Op::DictInsert` is.
- **Pinned by** `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` and `test_values`.

## The typed `sales` benchmark is written over `Dict`

**Status:** built.

- A typed benchmark and its Rust twin do the same work in the same data model (the mandate's "written entirely in ry's own types"), so `sales.typed.R` totals revenue per region in a `Dict<Str, F64>` via `Dict.update` and `tools/bench/typed/sales.rs` uses a `HashMap<&str, f64>`; a DataFrame pipeline measured the table engine and the R call protocol instead. Both weight regions alphabetically, so the checksum is the plain R program's.
- The column lists are held by untyped code, so each call converts the `List<Str>` and scans the numeric ones (`typed::type_of`); that boundary cost is in the ratio on purpose (`backlog.md`).

## Short strings are built and hashed as two words

**Status:** built.

- A string of at most 15 bytes is two words with inline bytes. `values::str_from_bytes` builds them from overlapping fixed-size reads with no `memcpy`, and `values::key_hash` mixes the two words. Equal inline strings have equal words because bytes past the length are zero. The crate is little-endian only (`compile_error!`), which the layout relies on.
- `typed::usable_string` skips UTF-8 validation for a CHARSXP with `VALID_UTF8` set.
- **Pinned by** `test_values::strings_switch_to_heap_past_fifteen_bytes`.

## The typed log reports each compilation's own time

**Status:** built.

- **Why.** Compile time is part of typed ry's performance bar (single-pass, far below `rustc`), so it must be visible to weigh optimizations.
- With `RY_TYPED_LOG=1`, `typed: compiled for [...] -> ... in N us` excludes nested callee compiles (`typed::NESTED_COMPILE`), and refusals log `typed: refused [...] after N us`, so a run's lines sum to its total. No clock is read without the log. The typed suite sums them into `compile_seconds` beside `rustc`'s time (`tools/bench/bench.py`, `typed_log`, `RustRunner.compile_seconds`).

## One thread-local holds every typed allocation pool

**Status:** built.

- `values::Pools` holds the free stacks, large classes and `live` counter in one thread-local, resolved once per `object_alloc`/`object_free` and passed down (`block_alloc_in`, `block_free_in`), because in `libR.so` each thread-local read is a `__tls_get_addr` call. Next step: backlog, "Typed ry against Rust".

## `Str.split_on` with a one-byte separator scans a word at a time

**Status:** built.

- `ry_str_split` with a one-byte separator counts first and cuts second, both eight bytes at a time: `values::separators_in_word` XORs with the repeated separator and uses the carry-free zero-byte test, which sets exactly the high bit of each match. The tail is read through `str_from_bytes`, never past the text; the list is allocated once at its final length and short fields are inline.
- **Rejected: `memchr`**, because delimited fields are short and a call per separator costs more than the scan.
- `box_free` walks the set bits of the `objects` and `strings` masks.
- **Pinned by** the sweep in `test_values::string_functions_follow_r_and_the_standard_library`.

## A branch on a comparison of two doubles compares once

**Status:** built.

- `backend::emit_block` recognizes a block ending in `Compare(op, Dbl, a, b)`, `Truth` and `Branch`, and emits the ordered comparison as the branch. A holding ordered comparison proves neither operand is NaN, so the unordered test runs only on the other path; `!=` holds for unordered operands, so there the unordered test comes first. The compare's and truth's locals are still set, because later blocks may read them.
- **Pinned by** `test_tiers::typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` (all six operators, and `0 / 0` with `!=` and `<`).

## `List.count_if` with a comparison predicate counts over the whole list

**Status:** built.

- **Shape.** In `Lowerer::walk`, a `count_if` over a `List<F64>` whose inline one-parameter predicate only compares the item with numbers (`Lowerer::compares_item`: comparisons against a double literal or `F64` local, joined by `&`, `|`, `!`, parentheses) lowers the body once with the parameter bound to the list, giving `Op::CountTrue` over the mask, which `typed::fusion` fuses. Other predicates keep the per-item loop.
- **Why only `List<F64>` comparisons.** Binding the list to the item name is sound only where every operation means the same elementwise; checked `I64` arithmetic may fail per item, and calls or `match` have no list form. `I64` over `List<I64>` is left out because its declared-`I64` lowering differs from a list comparison.
- **Kernel.** A one-step program tallies answers as it compares (`tally_compared`); the general path tallies a chunk's lanes in a byte.
- **Pinned by** `test_tiers::typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` and `test_values::fused_counts_follow_three_valued_logic`.

## The typed `array_filter` benchmark counts with `List.count_if`

**Status:** built.

- A typed benchmark and its Rust twin are both written as users would write them. The twin counts with `filter` and `count`, which `rustc` vectorizes, so `array_filter.typed.R` counts with `List.count_if` and a comparison predicate. Typed ry has no autovectorizer and gets none (compile time is single-pass), so whole-list operations are the fast spelling.

## Shapes are confirmed after lowering, and machine code is generated once

**Status:** built.

- `compile` lowers (`lower_settled`), confirms `Lowered::constructed`, and lowers again only if a `match` over any tag saw an older epoch; `generate` then runs unboxing, fusion, ownership and Cranelift once. Confirming after code generation made functions that construct shapes and match over any tag compile twice. Lowering is cheap to repeat.
- **Confirming before the code exists is safe**: a confirmed shape only adds a case to matches over any tag; if the backend then refuses, later matches test for a shape nothing constructs, which changes no answer.

## `List.sum` adds doubles in order in double precision

**Status:** built.

- **Decision.** `List.sum` over `F64` is the left-to-right double sum from zero. The interpreter folds with `+`, compiled code calls `values::ry_list_sum_dbl` (`Reduce::ListSum`), and a fused elementwise total adds each chunk in order (`values::add_in_order`), so all give the same bits. R's `sum()` (`Reduce::Sum`, `values::ry_sum_dbl`) keeps the long-double fold, because GNU R defines it.
- **Rejected: matching `sum()`'s long double**, which tied a ry function to an R kernel and kept the sum on the x87 stack where it cannot vectorize. **Rejected: a vectorized order**, because the interpreter, the chunked kernel and a user's loop would each need the same lane layout to agree; in-order is what every path gives without trying, as Rust's `iter().sum()` and Roc's `List.sum`.
- `List.sum([1, 1e-16, 1e-16])` is `1` where `sum()` gives the next double, as the reference states. `test_standard_library` pins the difference and `test_tiers` that the tiers agree bit for bit.

## Counts that share a list share one kernel pass

**Status:** built, except the "Not done" bullet on range facts, which "Whole-number ranges decide which checks typed code needs" supersedes.

- **Shape.** `Op::CountTrue { program, inputs, counts }` pairs several counted operands with their `I64` result locals and has no `dest`. `Inst::assigned` is the one place that says which locals an instruction writes, and every pass asks it. At most `values::COUNTED_LISTS` counts share a pass.
- **Merge rule (`fusion::merge_counts`).** A count merges into the nearest earlier count of its straight-line run when no `CallR` or `CallTyped` lies between them, nothing from the earlier count on assigns the later count's inputs or results, nothing between reads the later count's results, they read at least one common list, and the merged program fits the kernel limits. Identical steps are reused, so a shared comparison runs once.
- **Why a shared list.** A pass fails unless all its list inputs have one length; merging counts over unrelated lists would give up (and rerun) calls that each count alone would pass. A shared list ties the lengths, so the merged pass fails only where one count would.
- **Why a call keeps them apart.** A count fails on `NA` and gives up the call; moving it before an R call would run that call twice. Calls are the only effect a rerun repeats.
- **Why the later count moves up**: the earlier count's results may be read in between, and moving up needs only the later count's inputs unchanged over the gap.
- **Pinned by** `crates/typed/tests/test_fusion.rs`, `fused_counts_follow_three_valued_logic`, and the log-rank cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic`.

## Parsing a number is one instruction with two results

**Status:** built.

- `Op::Parse { string, parsed }` assigns the number to `dest` and whether it parsed to `parsed`, from one call of `values::ry_str_to_i64` or `ry_str_to_dbl`; the number is zero when parsing fails, and the instruction cannot fail. It replaced a test-then-parse pair that parsed every string twice. `Inst::assigned` reports the extra local.

## An `I64` remainder or quotient by a positive constant needs no checks

**Status:** built.

- **Shape.** When the divisor is a positive constant (`ssa::Builder::constant` reads its defining `iconst`), zero and -1 cannot happen, and a truncated remainder is negative exactly when the floored one differs: `%%` adds the divisor masked by the remainder's sign, `%/%` adds that sign. A power of two needs no division (low bits, arithmetic shift).
- **Why in the backend**: typed IR locals are variables and cannot say an operand is constant, while the SSA builder knows each value's definition. A constant assigned in another block is a block parameter until sealing and takes the general path.
- **Pinned by** the constant-divisor cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` (negative dividends, `Num.min_i64`, zero and negative constants).

## A split list that is only read at constant positions is never built

**Status:** built.

- **Shape.** `unbox::replace_split_lists` runs before `unbox::unbox` and replaces a group of list variables with variables for the string, the piece count, and the bounds of each piece some read names. `Op::SplitBounds` assigns them from one scan (`values::ry_str_split_bounds`) that stops after the last named piece; a read becomes `Op::Piece` (`values::ry_str_piece`), which fails exactly where the list read failed.
- **Which lists.** A group of `List<Str>` variables joined by copies, assigned only by splits or in-group copies, used only by such copies or by `Op::Get` with an index one `I64` constant assigns, from 1 to 32. A list returned, measured, passed on or read at a computed position is kept.
- **Why the variables hold the string**: locals are variables, so the split's string may be reassigned before a read; copying it at the split makes every read see its split's string on every path, as for a box's fields.
- **Why cut at the read**: a row rejected early never pays for later pieces (Rust's `split` yields slices too). A read run many times cuts each time; the common shape reads each piece once.
- **Pinned by** `crates/typed/tests/test_unbox.rs`, `check_split_bounds` in `crates/typed/tests/test_values.rs`, and the split cases of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text`.

## A loop that fills a list is fused with the loop that reads it

**Status:** built.

- **Shape.** `loops::fuse_loops` (`crates/typed/src/loops.rs`) moves the consumer loop's body into the producer at its `Push` or `Set`, which it deletes. The item passes in a variable, a counter stands in for the length (`List.len` still answers), and a three-stage pipeline fuses in two rounds. With no list holding them, the items' boxes are removable by `unbox::unbox`.
- **Which lists**: the module documentation lists the conditions (one counted producer filling once per iteration, a consumer reading first to last, nothing else reading but the length, a straight path between whose only branches give up, no calls or returns, no shared variables).
- **Why reordering is sound.** Without calls a body's only effects are on locals and the heap values they own, and the interference test (`Bits::intersects`) excludes shared variables. Every failure (`Terminator::Fail`, no payload) gives the call to the interpreter, which reruns it from the start. The one accepted difference: a consumer body that never ends, where unfused a later producer failure would have given up first.
- **Making the list must not fail**, because its creation is deleted: the capacity must be `never_negative`.
- **The store's own failure stays.** `Op::Set` into a double list gives up on `NaN`/`NA` (the backend's `Set` arm, giving the interpreter's "a list cannot hold NA"), so the fused loop keeps an `Op::IsNan` branch there. A `Set`-filled list of R integers or logicals has that failure with no IR test, so it is not fused.
- **Where the list may be made**: on the single-predecessor path into the producer's guard, up to `MAX_BETWEEN` blocks (`runs_into`); a `Set`-filled list must be made in the guard, since its length is the loop's.
- **Why before `unbox` and ownership**: no `Dup`/`Drop` exists yet to move, and the boxes become removable only after their lists are gone.
- **Supporting changes**: `List.map` with a named typed function inlines it at `apply` (`lower::typed_function`), because a loop with a call never fuses; `unbox::remove_unread` removes unread instructions that cannot fail; `Op::Piece` at index 1 cannot fail (`ir::Op::can_fail`).
- **Known limits** are in `backlog.md`.
- **Pinned by** `crates/typed/tests/test_loops.rs` (each condition, plus a fuzz against an IR evaluator; `FUZZ_SEED=<n> cargo test -p typed --test test_loops` replays), `an_unread_value_goes_with_what_only_it_read_unless_computing_it_can_fail` in `test_unbox.rs`, and the pipeline cases of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text`.

## Whole-number ranges decide which checks typed code needs

**Status:** built.

- **Shape.** `ranges::ranges` (`crates/typed/src/ranges.rs`) computes one interval per whole-number local, read by `backend::compile` through `Ranges::of(block, local)`. An `I64` `+ - *` whose exact result fits drops its overflow test; `%%`/`%/%` with a known-positive divisor drops the zero and -1 tests, and the floor correction when the dividend is known non-negative; `Op::Truth` and `Op::ToDbl` skip the `NA` test for an operand whose range starts above `NA`.
- **Finding a range.** A range covers every assignment of the local. Ranges grow from empty to a fixed point, a bound still growing after three rounds jumps to its type's limit, and up to three descending rounds recompute from that (`i %% 7` is at most 6 whatever the counter's end). A local some path reads before assigning includes 0, because machine code starts non-parameter locals at zero.
- **Branch narrowing.** A branch on a comparison of two `I64` locals narrows both in every block the target dominates, only when the branch is the target's only entry and the condition and operands are assigned at most once, before the comparison on every path. Then every way back into a dominated block leads through the comparison.
- **A wrong range is a miscompile**, so each transfer function follows what the backend emits: a length written to an integer keeps the type's whole range, extra assigned locals get their type's range, an operator that gives up on overflow is clamped to its type, and a logical or `Bool` is not assumed to be 0 or 1, because package C code can store another true value and `Op::Truth` passes it on. It relies on two IR contracts: `Op::Step` never overflows, and a `Bool` is never `NA`.
- **Rejected: SSA value ranges in `crates/ssa`**, which would have to recover the types and the checked operators from emitted code.
- **Known cost**: Cranelift places a pure value at its first use per dominator branch, so with the overflow test gone `i %% 7 + 1` can be computed twice; the removed checks outweigh it.
- **Pinned by** `crates/typed/tests/test_ranges.rs` (each rule, plus a fuzz checking every read value lies in its range; `FUZZ_SEED=<n> cargo test -p typed --test test_ranges` replays) and the range cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic`.
