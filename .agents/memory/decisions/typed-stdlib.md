# Decisions: Typed ry and the standard library

Typed closures and their native compilation, records, tuples and tags, and `std`.

## Beta semantics: Signature matching is name-aware

**Status:** built.

An R call site matches arguments by name. Matching an annotation against a definition by flat
position therefore routes a value to a wrongly-typed formal, which accepts code that crashes at run
time. Both the contract and the implementation match by name where names exist.

## Records, tuples and tags are ry values, not R lists

**Status:** built, except the lowercase `record{...}` and `tuple{...}` spellings, which "The DataFrame API is Polars', and ry's own types are UpperCamelCase" supersedes.

- **Previous shape (draft only):** a record or tuple was a plain list and a tag a classed list with
  an `ry_tag` attribute, so R and C code could read them unchanged. That made R's list semantics
  (attributes, partial `$` matching, `c()` coercion, `names<-`) the semantics of ry's own types,
  and gave the JIT no fixed layout to rely on.
- **Chosen shape:** one heap object kind of ry's own (a type code GNU R leaves unused) holding a
  32-bit interned layout id and eight-byte slots. The layout records the tag (an interned small
  integer), the field names sorted canonically, and per-slot storage: an unboxed length-one
  `double`, `integer` or `logical`, or a traced handle. Values are immutable with value semantics.
- **Types:** `record{...}` and `tuple{...}` are distinct from `list{...}`, since the values differ.
  Tags are structural; a tag union is an ordinary union, `..` makes it open, and there are no row
  variables, which keeps the checker Hindley-Milner.
- **R boundary:** explicit. `as.list()` converts out, `as.record()`/`as.tuple()`/`as.tag()` convert
  in. Each kind has an implicit class (`ry_record`, `ry_tuple`, `ry_tag`) for `class`, `inherits` and
  S3 dispatch. Printing, `identical`, `all.equal`, field access, the type predicates and builtins
  that only store a value (such as `list`) accept native values; every other builtin raises one
  error that names the conversion. C code sees an opaque object (`TYPEOF` 27): list element moves
  work and every other accessor raises the same error, because a deep scan at `.Call` would cost
  time in proportion to the data. `serialize` refuses them, so they never leave the session.
- **Layout:** keyed on tag and sorted field names only; which slots hold raw scalars is a per-object
  mask, so same-typed values share a layout id whatever their contents.
- **Impact:** plain R is untouched, because every new form is a syntax error in R. Field reads with
  a known type compile to a slot load after one layout-id comparison, and tag tests are integer
  comparisons. The design draft is `docs/src/content/docs/development/design/records-and-tags.md`.
- **Field read sites** (`bytecode::Op::Field`, `records::cached_field`). `x$name` with a symbol
  field compiles to a `Field` instruction that carries a per-site shape constant, the integer
  pair `[layout id, slot]` (`records::NO_LAYOUT` until a record is seen). Tier 0 compares the
  layout id and reads the slot; any other object takes `subset::dollar_value`, `$`'s own path. The
  JIT turns a filled cache into `Intrinsic::Field`: it guards record kind, no object flag and the
  layout id, then loads the slot unboxed when the mask bit is set and the NaN-box prefix matches
  the site's feedback, and hands off to tier 0 otherwise. `$` had to join `base_primitive`'s list,
  or every site compiled as a general call. 5,000,000 iterations of `s <- s + p$x * p$y + p$z`
  take 0.074 s on a record against 1.83 s on a list (0.055 s with locals), profiling build.
- **Item, constructor and `match` sites.** `x[[k]]` with a constant `k` compiles to `Op::Item`
  with the same `[layout, slot]` shape cache (`records::cached_item`); a list or vector subject
  falls through to `[[`'s path. Constructor calls (`.ry_record`, `.ry_tuple`, `.ry_tag`) whose
  names are distinct intern their layout at compile time and compile to `Op::Construct`; a
  payload-free tag is one shared constant. A guard-free `match` compiles to `MatchShape`,
  `MatchTag`, `MatchLiteral` and `Slot` tests, sets the arm's bindings only after the whole
  pattern matched (as the special does), and ends in `MatchFail` with the special's error; a
  `match` with a guard stays with the special. The JIT branches on kind, layout and tag natively
  and reads slots through `Intrinsic::Field`; `Construct` is an opaque pure step. Profiling
  build, against the build before these sites: 5,000,000 `t[[2L]]` reads 2.27 s to 0.026 s
  (`r$b` 0.026 s, list `l[[2L]]` 0.83 s); 2,000,000 three-arm tag `match`es 1.29 s to 0.57 s
  (`switch` on a list's `$kind` 1.10 s); 2,000,000 `:Ok(i)` constructions 0.69 s to 0.13-0.23 s
  (`list(kind =, v =)` 0.35 s).
- **Raw slots under NaN prefixes:** a raw integer or logical sits under its own quiet-NaN prefix, so
  the one-bit mask suffices and a slot read needs no layout lookup to know its storage. The object
  keeps its slot count in header padding, so the collector never reads the layout table.
- **The refusal is a builtin flag, not a scan:** `BuiltinDef::accepts_native` is set at startup from
  `syntax::native_values::ACCEPTS_NATIVE`, and `install_all` asserts every listed name exists, so a
  renamed builtin cannot silently drop off the list. The check tests only top-level arguments,
  because looking inside lists would cost time in proportion to the data.
- **The checker reads the same list.** It reports a record, tuple or tag passed to an R function
  that is not on it as a `type-mismatch`. The list lives in `syntax` because both `semantics` and
  `interp` already depend on that crate, so the static error and the runtime refusal cannot drift
  apart. The checker reports only when every member of the argument's type is the same native
  kind, and never for a function the project itself defines or shadows.
- **`$<-` on a record keeps its type.** Writing a field checks the value against the field's type,
  and writing a field the record lacks is an error rather than a widening, because the runtime's
  layout is fixed by the field names.
- **Pattern encoding reuses the value constructors** (`.ry_tag`, `.ry_tuple`, `.ry_record`), so a
  quoted pattern deparses with the same code as a quoted value, and `_`/`..` stay bare symbols.

## Inline annotations at run time

**Status:** built, except GNU R through `ry run` and `ry repl`, which "ry run and ry R: the command chooses the runtime" supersedes.

ry's runtime checks a function's inline parameter and return types at its boundary, and erases
binding annotations. GNU R, through `ry run` and `ry repl`, reads the erased program. The contract
is the typing reference, "Annotations at run time".

**Why check at all.** An annotation that the runtime ignores can be false forever without anyone
noticing, because the checker trusts it at every call site in other files. A boundary check turns
a wrong annotation into an error at the call that broke it. It also gives the JIT a type guarantee
inside the body: after entry, a `double` parameter is known to be stored as logical, integer or
double, so a guard on it is redundant.

**Why parameters are checked when forced.** Checking on entry would force every annotated
argument, which changes R semantics (laziness, `missing()`, `substitute()`, a never-used argument
that would fail). The promise carries `gp::CHECKED_PROMISE`, and `force_promise` runs the check
before storing the value: a failed check leaves the promise unforced and still checked, so code
that catches the error cannot read an unchecked value, and the JIT guarantee below holds. A promise
forwarded through `...` is pending in several typed frames, and forcing it checks every one; the
bit is cleared only once no frame still holds it. A value that is already computed (a constant, a forced promise) is
checked on entry, because there is no later moment to hook.

**Why storage masks.** A check is one bit test of the value's `SEXPTYPE` against a `u32` mask
(`crates/interp/src/annotations.rs`). Each mask is at least as loose as the static type (vector
shape ignored, the numeric promotion order admitted, an alias, class, type variable, `Any` or frame
unchecked), so a value the checker would accept never fails at run time. Element types and
attributes would need a walk of the value, which is not cheap enough to run on every call.

**Where the signature lives.** Lowering a `function` registers its masks in a map keyed by the
formals pairlist (or the body, for a function without parameters), which closures share by pointer,
so a closure copy (`environment<-`) keeps its checks and `formals()`, `body()` and `deparse()` see no
change. The signature also holds the definition's body (and the formal names, for messages), and a
closure matches only when its body is that same object: `function` builds closures from whatever
formals it is handed, so `rlang::new_function(formals(f), body)` would otherwise inherit `f`'s
checks. The key carries `gp::TYPED_DEFINITION`, so an untyped call pays one header bit test. The map
holds a weak reference per entry, whose key identity guards against a dead object's address being
reused; dead entries are swept when the map doubles. A function without parameters whose body is a
constant or a symbol has no unique key and is not checked.

**Why bindings are not checked.** `x: T <- v` would need a check on every assignment and a choice
about later reassignments, and the checker already reports a wrong value statically. Erasing it
keeps assignment as cheap as plain R.

**The JIT.** A compiled call site whose argument types the checker proved can skip the check, because
the mask test is pure and its failure path is an ordinary R error. The pending-check stack
(`annotations::State::pending`, popped by frame depth at closure exit) is the only state a compiled
frame has to honor.

**Opt-out and strict mode.** `[check] inline-annotations = false` erases annotations and registers no
checks (`RuntimeFlags::annotation_checks`), so the setting means the same thing in analysis and at
run time. Before running, `ry run` and `ry repl` check the code as a one-file project
(`crates/ry/src/preflight.rs`): they print the `inline-annotation` and `annotation-mismatch`
warnings, and in strict mode they refuse code with an error. Other findings stay `ry check`'s, so a
plain script runs exactly as before; a script with no inline annotation and no strict mode skips the
analysis.

**The console sees whole commands.** The editor hands the console filter one line at a time, so the
filter holds an unfinished command and releases it whole. "Unfinished" is
`syntax::input_is_unfinished`: every syntax error is marked `incomplete` (an opener never closed,
or raised with only trivia left), which the parser knows and a range check cannot, because an error
at a trailing newline is reported on the last code character. ry's own console uses the same rule. Otherwise strict mode could refuse the last line of a definition whose
first lines R had already read, leaving R mid-command.

**The console preflight checks each command against the session's interface**
(`semantics::session`, `crates/ry/src/preflight.rs`).

- Previous shape: the session was one growing script file, re-parsed and re-analyzed whole for
  every command, so a typed paste of n lines cost O(n²) (1,000 lines took 3.6 s). The session
  text duplicated what had run, and a paste cut short by Ctrl-C stayed in it.
- Target shape: each accepted command is a `SourceFile` of its own with `command: Some(index)`.
  A log in a singleton salsa input, which grows by `commit` and shrinks from the end by
  `truncate`, (`ConsoleSession`) records what each command
  binds, declares and defines, its `# typing:` directive and whether it used ry syntax. Name
  resolution, type definitions, the typing mode and "holds ry values" fall back to the log for
  commands before the reader. Those reads are untracked on purpose: they only look at smaller
  indices, and the log changes only past the reader or by dropping commands after it.
- The exception is a read inside a function body, which sees the latest binder, as R's global
  environment does. It goes through a `SessionBinder` salsa input per name, created when a
  command first reads the name that way. `commit` sets it and returns the earlier commands that
  read the name, and transitively those whose function bodies read a name such a reader binds,
  because a caller's type follows its callee's. Strict mode rechecks the ones that passed a check
  when they joined (input that joined unchecked may hold old errors); a strict error there
  refuses the new command, and
  `truncate` undoes the commit. Tracking immediate reads too would make each rebinding
  invalidate a chain of earlier commands and bring the quadratic cost back.
- The command files are not in `ProjectFiles`: a project-wide fold over them would cost
  O(session) per command.
- Rollback: an error never cuts a paste short (R gets one line per chunk), so only Ctrl-C drops
  input. The console counts the dropped lines in `pending` and calls `InputFilter::discard`;
  `ConsoleFilter` maps lines back to the command's bytes (identity for ry's runtime, whose
  elaboration keeps lines; `syntax::erased_lines_end` for GNU R's erased program), and
  `Preflight::retract` replaces the last command with the prefix that ran, unchecked.
- Impact: a 5,000-line typed paste takes 1.4 s raw, linear; findings name the command they are
  in, with lines counted from its start; a `# typing:` directive governs only later commands.

## Ry's standard library (`List`, `Str`, `Num`, `Result`, `Dict`, `Set`)

**Status:** built, except the `ry` namespace as the library's home at session startup, which "The standard library is the `std` namespace, with an optional prelude" supersedes.

- **Previous shape:** none. Results were a user-declared `@alias`, and the only stdlib was the stubs
  for R's own packages (the first record in this file), so every project re-declared `Result` and
  wrote its own helpers over plain lists.
- **Names:** plain qualified names such as `List.map` are ordinary R symbols with a dot, bound in a
  `package:ry` environment attached just above `package:base` (`standard_library::attach`); session
  startup moves them into the `ry` namespace, attached in front of the default packages beside the
  frame functions (see "Frames as runtime values"). There
  is no static dispatch, so a call is a symbol lookup and the checker reads one stub per name. A
  definition in the user's code shadows a library name like any R binding, and
  `test_standard_library.rs` asserts that no shipped package's `.exports` holds a library name, so
  plain R cannot change meaning. Module names are Roc's; a Roc name that already means something
  else in R is renamed (`Result.try` is `Result.and_then`).
- **One source of truth per side:** the types live once in `types/std.Rtypes` (namespace `std`,
  always active, in scope by bare name through the prelude), and the runtime functions live once in `crates/interp/src/standard_library.rs`,
  registered with `BuiltinDef::accepting_native`. A test asserts the two name sets are equal and
  that every declared parameter name is accepted at run time.
- **`Dict` and `Set` are native kinds** (`Fields::Dict`, `Fields::Set` in `crates/heap/src/records.rs`)
  with value semantics, insertion order and an open-addressing index, because a named list
  matches partially and allows duplicates, and an environment has reference semantics.
  `identical()` ignores order, as Roc's equality does. Updates reuse a uniquely held value in
  place (see "Standard library updates reuse a uniquely held value").
- **Errors are tags, not conditions:** a function that can fail returns `:Ok(x) | :Err(:Tag)`, and a
  wrong argument type raises an R error that names the parameter, so a misuse is loud and a domain
  failure is a value.
- **Impact:** no change for plain R (no shipped name collides, and all names are new symbols). The
  checker, hover, completion and goto-definition read the stub file like any other, so incremental
  analysis gains nothing new to invalidate. The user contract is `reference/standard-library.mdx`;
  the draft with open questions is `development/design/standard-library.md`.

## Standard library updates reuse a uniquely held value

**Status:** built.

- **Previous source of truth.** Every update (`Dict.insert`, `Set.insert`, `List.append`, ...)
  built a fresh value, because a builtin could not know whether its caller would overwrite the
  argument's binding. A loop growing a collection was quadratic: 40,000 `Dict.insert`s took 17.4 s.
- **Target shape.** Perceus-style reuse, as Roc and Koka do it. `eval::rebind_call` recognizes
  `x <- f(x, ...)` for a builtin flagged `reusing_first_argument` and passes the first argument as
  `Call::reusable` when the frame binding holds its only reference and no other argument is the
  same value; `standard_library::owned` also accepts a reference-count-zero temporary. Dicts and
  sets are counted records whose key and value vectors are resizable and double when full, and
  `build_index` takes spare room so `reserve` can presize the index. The bytecode compiler hands
  such assignments to the AST evaluator.
- **Why not a special form or last-use analysis.** A special form would make every library update
  syntax, and last-use analysis needs the compiler; the reference count is exact at the call
  boundary for AST frames, so reuse is decided where it is cheapest. It is not exact for a frame a
  compiled closure owns: bytecode registers hold uncounted references (`backlog.md`). The remaining copies (a parameter's
  first update, `state$seen <- Dict.insert(state$seen, ...)`) are in `backlog.md`.
- **Impact.** Value semantics are unchanged (any second holder copies). 40,000 inserts: 0.04 s.
  The checker is unaffected.

## Standard library breadth against Roc's builtins

**Status:** built, except the skipped wrapping arithmetic and the R-sized limits and conversions (`Num.max_integer`, `Num.to_integer_checked`), which "Typed ry has its own numbers, `Bool`, `Str` and `List`" supersedes.

- Each module was compared with Roc's builtins (pre-`Try` API) and everything that is typed,
  `NA`-free and reports failure as a tag was taken; the full list and the renames are in
  `development/design/standard-library.md`.
- **Skipped, with the reason:**
  - `Box`, `Encode`, `Decode`, `Hash`, `Inspect`, and the `Eq` implementations (`Dict.is_eq`):
    they serve Roc's memory layout or abilities, which ry does not have; `Bool.is_eq` compares any
    two values by `identical()`.
  - Wrapping arithmetic (`add_wrap` and the rest): R's 32-bit integer reserves its minimum for
    `NA`, so wrapping would have to stop at that one value, and the code that wants wrapping
    (hashing) wants it total.
  - Per-width conversions and limits (`to_i8`, `min_i32`, `int_cast`, `f64_to_parts`, `Dec`):
    R has one integer and one double type, so the limits are the four unsized
    `Num.max_integer`, `min_integer`, `max_double`, `min_double`, plus `Num.infinity`; Roc's
    `nan_f64` is skipped because no library function takes or returns `NaN` on purpose.
  - Effectful `!` variants (`List.walk!`): R does not track effects; `List.for_each` covers them.
  - `Str` capacity functions: R strings are immutable and interned, so there is no buffer to
    reserve or release.
- **Taken later:** `List.range` (`seq` returns a vector, not a `list[integer]`),
  `List.keep_if_try`, `Num.to_integer`, and `Dict.capacity`, `Set.capacity` and
  `release_excess_capacity` on `List`, `Dict` and `Set`. They were first skipped on the claim
  that the heap shrinks collections, but it never shrinks a resizable vector, so after a
  `reserve` the room stays until the value dies. `Dict.with_capacity` and `Set.with_capacity`
  came last: an empty collection has no key vector until its first key fixes the key type, so
  its open-addressing index, sized for the requested room, carries the room
  (`standard_library::empty_room`), and the first insert allocates the key and value vectors
  with that capacity. `Dict.reserve` on an empty dictionary works the same way.
- **`NaN` is never a result.** A `NaN` from an unchecked function (`Num.sin(Inf)`,
  `Num.pow(-1, 0.5)`, a `List.sum` of `Inf` and `-Inf`) is an error, and a `_checked` function
  returns `:Err(:Undefined)`, which its signature names.
- **Unchecked operations error where Roc crashes or returns `NaN`.** `Num.add`, `div`, `sqrt`,
  `log`, `asin`, `acos`, `pow_int` and the rest were first skipped because they produce `NaN` or
  overflow for part of their domain. They are taken with an R error there that names the
  `_checked` variant, so a step can be passed by name (`List.walk(xs, 0L, Num.add)`) and a domain
  failure is still never a silent `NaN`. `Num.is_nan` is the one function that accepts `NaN`.
- **Bit operations** (`bitwise_*`, shifts, bit counts) work on the 32-bit two's complement and
  error only on the pattern `-2147483648`, which is `NA`. Shifts wrap as Roc's do.
- **`Bool` and constants.** `Bool` was first skipped as redundant with `TRUE`, `!` and `&&`; it is
  taken so boolean steps can be passed by name and Roc code reads the same. `Bool.true`,
  `Bool.false`, `Num.pi`, `Num.tau` and `Num.e` are values in `standard_library::CONSTANTS`,
  bound in `package:ry` by `standard_library::attach` and declared in the stub as plain types.
  Under GNU R, `crates/ry/src/load.rs` reads which `std.Rtypes` declarations are not function types
  and binds each as an active binding that stops with the "only ry's runtime" error.
- **UTF-16 and UTF-32** (`Str.to_utf16`, `from_utf16`, `to_utf32`, ...) follow Roc's newer `Str`
  builtins, with `:BadUtf16` and `:BadUtf32`; NUL is bad because an R string cannot hold it.
- **Changed:** `Num.ceiling`, `floor` and `round` keep the number type (R integers are 32-bit, so a
  rounded large double cannot become an integer); `Num.to_integer_checked` converts with
  `:Err(:OutOfBounds)`. Saturating integer arithmetic clamps to plus or minus 2147483647.
- **Hover docs** come from `#'` lines directly above a declaration in a `.Rtypes` file
  (`StubLibrary::documentation`); a later source's first declaration of a name replaces or clears
  them, the same override rule as the type.

## The standard library runs only on ry's runtime

**Status:** built, except the check-time diagnostic for library calls in a project that targets GNU R.

- A pure-R build for GNU R was declined. Many functions return result tags and `Dict`/`Set`
  values, which GNU R cannot represent, and a partial build covering only `Str` and `Num` would
  split the library into two dialects with different availability.
- Instead, a diagnostic should flag library calls once a project can declare that it targets GNU R
  (`backlog.md`).

## Nominal and structural types cover both product and sum types

**Status:** built.

- **Shape:** every record, tuple or tag union is structural by default (`alias` only names it), and
  `type Name = ...` (or `#: @type`) wraps it in a nominal type. That gives a 2x2 grid, structural or
  nominal for product and for sum types, with one mechanism instead of a separate "declared record"
  or "declared union" concept. The user guide teaches it in
  `guide.mdx` (a record pair and a union); the contract is the reference section "Nominal records,
  tuples and tags".
- **Construction:** `@Name(...)` is the only constructor, and its parentheses are the value's own,
  so `@Point(x = 1, y = 2)` is a record and `@UserId(id = 1L,)` a one-field record. A bare value of
  the right shape is refused, which keeps construction in one checked place, like Roc's opaque
  `@Name`. A separate constructor syntax (`Point(x = 1)`) was rejected because it collides with R
  calls. `@Point((x = 1, y = 2))` still works, because the inner parentheses are only R grouping, but the docs never write it.
- **Previous weakness:** the checker handled the nominal wrapper only at assignment. `match`
  exhaustiveness, pattern checks, field writes, `$` completion and the native-value-to-R check all
  looked at the nominal head and saw no shape, so a nominal union was never checked for coverage,
  bindings lost their nominal type, and field writes went unchecked.
- **Target:** each of those reads projects the nominal value to its representation through the
  visible type definitions (`exhaustiveness::structural`, `Checker::structural`, `ide::representation`),
  so the definition stays the single source of truth for the shape. Binding patterns keep the
  nominal type. Exhaustiveness splits on constructors only while a row has a constructor head, which
  is what lets a recursive `type Tree<T>` terminate.
- **Impact:** no new stored state, and the projection reads per-name definitions that each item
  already depends on, so incremental analysis is unchanged. The `nominal_grid` typing group, the
  nominal cases in `crates/ide/tests/ide/records-and-tags.R.test` and `crates/interp/tests/test_records.rs`
  pin the behavior.

## An Array is a flagged R vector, written `[...]`

**Status:** built, except the type name `Array<T>` and its R item types, which "Typed ry has its own numbers, `Bool`, `Str` and `List`" supersedes.

- **Previous shape.** ry had no strict vector. Typed code used `c()`, whose result R coerces (`c(1, "a")` is character), recycles, and lets hold `NA`, so neither the checker nor a DataFrame column could promise one item type without missing values.
- **Target shape.** `[a, b, c]` is ry syntax for `Array<T>`, the standard library's strict vector: one exact item type (a single `logical`, `integer`, `double` or `character`, or a ry value), no `NA`, no attributes, no recycling and no implicit conversion. The contract is `reference/type-system.mdx`, "Arrays"; the tutorial mentions it in `guide.mdx`.
- **Runtime representation.** An Array is an ordinary R vector carrying the heap flag `flag_bits::ARRAY` (`crates/heap/src/object.rs`): atomic storage for scalar items, a VECSXP for ry-value items. A separate object type was rejected because every C API accessor, `sum()`, and the JIT already read the unboxed storage, so converting to R is free (drop the flag) and nothing has to learn a new layout. Setting any attribute clears the flag, which keeps "no attributes" true without checks at every attribute write. `values::duplicate` and `shallow_duplicate` drop the flag on the copy (nested Arrays keep theirs), because a copier is about to write and a builtin or C code may write an `NA`; only `arrays::writable` re-flags a copy, after checking the write.
- **One place enforces the rules.** `crates/interp/src/arrays.rs` validates construction, operators (`arith.rs` wraps each builtin in `strict`), indexing and replacement, then flags the result. The bytecode fast paths and the JIT's type guard refuse a flagged operand, so they can never skip a check; a dedicated Array feedback kind is backlog work.
- **R code keeps working on Arrays.** Reads follow R's positions wherever R gives no `NA` (negative positions, zeros), and everything that would produce `NA` is an error. A replacement that is not an item of the item type (an `NA`, an R vector, another type) does not error: the result is the plain R vector R would build, so base functions such as `ifelse()` that assign into an argument keep working. An operator works the same way: an operand that is not an Array and not a single item of the item type makes R's operator run and return a plain vector (`arrays::check_operands`), because base `str()` computes `ob - signif(ob, digits)` and `signif()` returns a plain vector. Typed code cannot reach that path because the checker rejects the value; the rules the checker cannot see (bounds, no recycling) stay runtime errors, so a typed `Array<T>` never holds a plain vector.
- **`List` results.** A `List` function returns an Array only when its collection argument (`list`, `first`, `lists`) is one, never because an item argument is. A callback's result type is bound by the `item` constraint (`types/std.Rtypes`), so the checker rejects `List.map` to a non-item instead of failing at run time.
- **Checker.** `Array<T>` fits `T[]` (atomic items) or `list[T]` (ry-value items), never the reverse, so Arrays pass to R code and R vectors need `as.Array()`. A DataFrame stores an Array column as its plain vector, so `column_type` maps `Array<T>` to `T[]`.
- **Impact.** No duplicated state: the flag is the single source of truth for strictness, and the checker's type and the runtime flag agree by construction. Pinned by `crates/semantics/tests/typing/arrays.R.test`, `crates/interp/tests/test_arrays.rs` (including a generated round trip) and Array seeds in both fuzzers.

## Array storage stays row-major; record Arrays go columnar only with JIT field fusion

**Status:** built.

- **Scalar Arrays already have the data-oriented layout.** An `Array<double>` is one contiguous `double` buffer with no NA and no attributes, which is exactly what a hot loop wants: `x[[i]]` is a bounds check and a load, and the JIT's Array guard can drop the NA and attribute checks (`decisions.md`, "An Array is a flagged R vector"). Nothing in that layout changes.
- **The open case is an Array of records.** `Array<{x: double, y: double}>` is a list of record objects, one 48-byte object per item whose length-one numbers are already unboxed into slots (`crates/heap/src/object.rs`, `Slot`). A columnar shape (one atomic buffer per field) would save the per-item header and pointer, cut collector work from one object per item to one per field, and make `List.map(xs, \(p) p$x)` a column read.
- **Why not now.** Every `xs[[i]]` on a columnar Array must allocate a record to hand out, so the interpreter's element loop trades a pointer load for an allocation, and a list a builtin or `.Call` code reads with `VECTOR_ELT` would need a materialized copy. The layout pays only once the JIT fuses `xs[[i]]$field` into a column load, so it waits for that, and DataFrame already serves the columnar-records workload.
- **Trigger to revisit.** When the JIT compiles `xs[[i]]$field` on Arrays, measure a million-item record Array loop row-major against columnar (`backlog.md`, "Columnar record Arrays"), and switch only if it pays.

## In-place updates through nested targets

**Status:** built.

- **Before.** Only `x <- f(x, ...)` with a builtin flagged `reusing_first_argument` updated in
  place, so `state$seen <- Dict.insert(state$seen, k, v)` copied every step: the list element was
  a second holder. Compiled code had a hole as well: registers hold values without counting as
  references, so an update from outside the body (`eval.parent()`, a forced promise) could change
  a value a register was about to use, and `r <- list(x, h())` with `h` running
  `eval.parent(quote(x[[1]] <- 99))` gave `r[[1]][[1]] == 99`.
- **Nested targets.** `rebind` accepts a target that is a chain of `$` or `[[` with a constant
  name, string or number (`same_expression` compares both sides). `solely_held` walks the chain
  requiring one reference at each plain list, and no other argument may be the value or a list
  on the chain, because argument values are not counted (`holds_on_chain`).
- **Bytecode.** `CallRebinding` carries the target so compiled code takes the same path. It is
  appended last because `OPS` is indexed by discriminant. The JIT treats it as a generic call and
  inlines it only when the target's slot type is plain.
- **Register rule.** `gp::COMPILED_FRAME` marks an environment while a compiled body runs on it.
  `reuse_permitted(env, value)` allows an update when the flag is clear, when the innermost frame
  is that environment's own compiled closure (whose updates the compiler orders with
  `may_modify_in_place`), or when no live register holds the value. A stale register only costs a
  copy. Applies to `complex_assign` too, so it fixes plain R as well as the library.
- **Record fields.** `r <- (..r, field = f(r$field, ...))` lowers to `.ry_update(r, ...)`, and
  `rebind_call` routes it to `rebind_update`, which evaluates the field's call with `r$field` as a
  nested target, so the record is a list on the chain. That needs records to count their
  holders: an interpreter-made plain record is a counted record (`records::allocate`) whose slots
  `set_record_slot` keeps counted, while tuples and tags stay saturated. The fields given after
  the updating one must be constants, because their code could otherwise see the field
  half-updated. When the record is still solely held afterwards, the new values go into its slots;
  otherwise the update builds a new record (`records::with_slots`). In a slot frame the compiler
  leaves such an update to `CallRebinding` instead of evaluating its arguments into registers
  (`eval::updates_own_field`), and the call makes the frame's environment. 2e6 updates of
  `List.append(r$values, x)` inside a function take about 0.7 s.
- **Moving a value into a helper's parameter** was first rejected as unsound and is now built
  with escape checks and an undo journal; see "Moving a collection into a helper's parameter".
- **Collection storage stays as it is.** A dictionary or set is already struct-of-arrays: a key
  vector, a value vector and an open-addressing index in an integer vector, all in the collector's
  heap, which a `SEXP`-holding value must be (`decisions.md`, "Data-oriented design and explicit
  allocators"). Hashing and probing were at most about 15% of a call, and the rest was call
  overhead, which "Library leaves resolve at compile time" removes.
- **Impact.** Value semantics are unchanged (pinned in
  `crates/interp/tests/test_standard_library.rs`, including the cases that broke the move), nested
  accumulator loops become linear, and incremental analysis is unaffected.

## Library leaves resolve at compile time

**Status:** built.

- **Before.** A call such as `Dict.len(d)` in a loop looked `Dict.len` up through the
  environment chain, built an argument list with a promise per argument, and dispatched through
  the builtin table on every iteration: about 0.27 us for `Dict.len` and 0.45 us for `Dict.get`
  on the profiling build (0.6 us on an older build), against 0.001 us for an empty iteration.
- **Tier 0.** The bytecode compiler resolves a qualified library name (`library_function` in
  `bytecode.rs`) to its builtin when the name resolves to a native-accepting builtin and every
  argument is positional with no `...`, and emits a guard on the symbol's binding, the builtin as
  a constant and a direct `CallBuiltin`. A failed guard (someone rebound `Dict.len`) falls back to
  the general call, so redefinition still works.
- **Leaves.** `standard_library::is_leaf` names the functions that never call back into R:
  every `Result`, `List`, `Dict` and `Set` function that takes no callback and does not update
  its first argument in place, and every `Num`, `Bool` and `Str` function except the `Str` walks.
  The JIT treats a leaf as an opaque step with `Effect::PureUnlessObserved`, so it flushes nothing
  around it unless an argument is observed: every boxed argument's object and ALTREP flags are
  checked at run time, because reading an ALTREP vector (`List.from_vector` materializes one)
  runs its class's methods, which a package may write in R. A set flag flushes state exactly as
  an opaque call does. An in-place update is excluded because compiled code may hold its argument in a
  register that no reference count sees, so the update could change a value still in use. The
  test `leaves_are_the_functions_without_callbacks` derives the rule from the stub signatures.
- **Intrinsics.** `Dict.len`, `Set.len`, their `is_empty`, `Dict.contains`, `Set.contains` and
  `Dict.get` compile to calls of small runtime helpers (`jit_collection_count`,
  `jit_collection_position`, `jit_dict_get`) that take the key unboxed and return an unboxed
  count or position. Anything they do not handle (a wrong type, `NA`, a mismatched key kind, a
  one-entry collection stored without an index) hands the instruction to tier 0, which raises the
  same error as before. `Dict.get` still allocates its `:Ok(value)` record; `:Err(:KeyNotFound)`
  is one shared value (`standard_library::Tags`).
- **Hash.** `key_hash` applies MurmurHash3's 64-bit finalizer after FxHash. FxHash's low bits are
  constant for doubles that hold whole numbers, which the open-addressing index uses to pick a
  slot, so integer-valued keys clustered and a miss probed linearly (0.80 us; 0.09 us with the
  finalizer alone, before the intrinsics). The
  index is not serialized, so the change needs no migration.
- **Measured** (`ry run`, profiling build, minimum of 7 runs of 1,000,000 calls on a 1,000-entry
  dictionary, us per call, before and after): `Dict.len` 0.271 to 0.009, `Dict.get` 0.450 to
  0.053, `Dict.contains` 0.320 to 0.018, a missing key 0.800 to 0.025.
- **Impact.** Library calls in loops are as cheap as `length()`, value semantics and errors are
  unchanged (pinned in `test_standard_library.rs`, including compiled-loop error cases), and the
  analysis is unaffected.

## Library traversals compile to loops

**Status:** built.

- **Before.** `List.walk`, `List.map` and the other callback-taking library functions called the
  callback through `eval::apply` once per element, which built a call, matched arguments, made a
  promise per argument and entered the callee's compiled body: about 5,000 instructions per
  element under callgrind, of which the callback's own `a + x` was about 1,000. Compiled code
  treated the whole library call as one opaque step.
- **Interpreter.** `standard_library::Callback` checks a closure callback once per library
  call: when it has no `...` and takes exactly the arguments the library passes, each item binds
  positionally with `bind_arguments` and `run_closure`, skipping argument matching. The frame's
  call still quotes the values, `(f)(5)`, as `eval::apply` does. A call naming the formals, like
  GNU R's `FUN(X[[i]], ...)`, would be wrong: `sys.call()` and `match.call()` users evaluate it
  in the caller's frame, where the formal names find the caller's own variables. Any other
  callback goes through `eval::apply` as before.
- **JIT.** `call_builtin` turns `List.walk`, `List.map`, `List.keep_if`, `List.drop_if`,
  `List.any`, `List.all` and `List.count_if` into `Action::Traverse` when the callback inlines:
  a closure literal at the call site (`Ty::Closure`), a function constant (`Ty::Fun`), or a name
  whose variable lookup from the enclosure finds a closure now. The element's type is the join
  of the feedback of the callback's own reads of its last formal (or a declared scalar
  annotation), and the callback body lowers through `lower_expression`, the same inliner direct
  calls use, so it must be scalar arithmetic, comparisons and intrinsics over its formals and
  loaded variables.
- **Guards.** The loop hands the whole call back to tier 0, which reruns it from the start,
  whenever the list is not a plain `VECSXP` (an ALTREP or array list), an element is not the
  expected unboxed scalar, a predicate returns `NA`, or the arithmetic overflows. A named
  callback adds an identity check of the register against the inlined closure, plus a
  speculation on its binding that keeps the closure alive (so a recycled address cannot pass
  the check) and recompiles once the binding changes. A hand-off before the loop has written
  anything visible is invisible; `map` and `keep_if` build their result in the destination
  register, which tier 0 overwrites.
- **Measured** (`ry run`, profiling build, best of 3). A hot caller that traverses a 100-element
  double list 10,000 times: `List.walk` 0.014 s (0.008 s with a named callback), `List.map`
  0.036 s, `List.keep_if` 0.013 s, `List.any` 0.008 s, `List.count_if` 0.008 s, against
  0.34-0.46 s with the JIT off and 0.57-0.94 s for the `Reduce`/`lapply`/`Filter`/`vapply`
  equivalents on GNU R. A cold caller over 1,000,000 elements only gets positional binding:
  `List.walk` 0.734 to 0.463 s, `List.map` 0.516 to 0.406 s, `List.keep_if` 0.488 to 0.350 s,
  `List.find_first` 0.501 to 0.324 s, `Dict.walk` (x10 over 100,000 entries) 0.670 to 0.598 s.
  The typed benchmark folds use no library callbacks and are unchanged.
- **One loop, described by a struct.** `analysis::Traversal` is a fold (`Walk { until }`, `Map`,
  `Filter`, `Any`, `All`, `CountIf`, `Find { index }`), a source (`List`, `Dict`, `Set`) and
  three flags (`backwards`, `indexed`, `from`), and `Traversal::of` decodes every looped name
  into it, so `List.walk_backwards_until` or `List.find_last_index` is a combination rather than
  a new code path. Codegen emits one loop: a list reads the `VECSXP`, a `Dict` or `Set` gets its
  keys vector and values list from `jit_collection_parts` (count `-1` hands off), `backwards`
  counts down, `indexed` passes the 1-based position as an integer, `from` takes an integer or
  whole double start of at least 1, and a find stops at the first match and builds its
  `Result` through `jit_found` after the loop.
- **Until steps.** A `walk_until` body lowers through `lower_step` into a (break, value) pair:
  `:Break(s)` and `:Continue(s)` give a constant flag, `{`/`(` unwrap, and `if` merges both arms
  with `Action::Select`. Any other body shape refuses, so the call stays generic.
- **`if` in inlined bodies.** `lower_expression` lowers `if (c) a else b` with a logical `c` to
  `Action::Select`, which evaluates both arms and picks one; that is sound because an inlined
  body has no effects, and an `NA` condition hands off so tier 0 raises R's error.
- **Unused formals.** A callback formal whose value cannot be unboxed (the key of a
  string-keyed `Dict`, say) binds to `UNUSED`, so a body that reads it refuses instead of
  resolving the name to a frame variable of the caller.
- **Impact.** Values, errors and warnings are unchanged (`traversals_agree_across_tiers` in
  `crates/jit/tests/test_tiers.rs` runs each shape in both tiers, including the hand-off cases;
  `fuzz_stdlib_traversals_agree_with_model` compares random traversals in every tier against a
  Rust model of the library), and analysis is unaffected. What still runs each callback through
  `standard_library::invoke` is in `backlog.md`.

## The standard library is measured against GNU R, function by function

**Status:** built.

- **Previous state.** Nothing compared a library function with what R code would write instead,
  so several functions were quadratic without anyone noticing: `List.len`, `List.get`,
  `List.first` and the other accessors copied the whole list into a `Vec` per call (8.6 s for
  100,000 `List.is_empty` calls), and `Dict.update` rebuilt the dictionary per call.
- **Target.** `just bench-stdlib` (`tools/bench/internal/stdlib`) times every `List`, `Str`, `Dict` and
  `Result` function at 1e6 against the fastest of base R, purrr, stringr and stringi, with
  checksums, and `tools/bench/internal/stdlib/results.md` holds the last full run. A row slower than R
  is either fixed or listed in `backlog.md` ("Open: standard library rows slower than GNU R")
  with its reason. Where the input is a vector, ry's side also runs on an Array, because R's
  vectorized functions work on unboxed storage too and the fair comparison is ry's unboxed
  vector.
- **Accessors read in place.** `list_count`, `list_item` and `list_slice` read a list or an
  Array without materializing it, so the accessors, `take_*`/`drop_*`, `sublist`, `split_at`
  and `starts_with`/`ends_with` cost what they return. `List.contains` compares a number by
  value against an Array's storage or a list's plain scalars, which is what `identical`
  decides for a number that is not `NaN`. A length-one Array is never such a plain scalar,
  because `identical([1], 1)` is `FALSE`.
- **Leaf callbacks skip the call.** A callback that is a leaf builtin (`Str.trim` passed to
  `List.map`) is called through its `BuiltinFn` directly (`Callback::leaf`, `call_leaf`), not
  through `eval::apply`, which built a call and matched arguments per element. A leaf reports
  errors against its own function object, and `call_leaf` swaps in the call `eval::apply` would
  have built only when an error escapes, so the condition is unchanged.
- **Updates in place.** `Dict.update` and `List.update` reuse their first argument
  (`reusing_first_argument`) and update an owned value in place after the callback returns,
  re-checking ownership then, because the callback may have stored the collection.
  `List.update` holds a reference on the list while the callback runs, as an R closure's
  argument would, so a `<<-` assignment into the list inside the callback copies it, and a
  callback that returns the list itself takes the copying path so the list never holds itself. Removal
  through `Dict.update` and `Dict.remove` still rebuild, because a `Dict` keeps insertion order
  and an ordered removal shifts every later entry.
- **Impact.** Values and errors are unchanged (the docs tests run every library example, and
  `fuzz_stdlib_traversals_agree_with_model` feeds Arrays as well as lists); the accessors went
  from O(n) to O(1) per call and `Dict.update` from O(n) to amortized O(1). Analysis is
  unaffected.

## Moving a collection into a helper's parameter

**Status:** built.

- **Before.** `d <- helper(d, k)` shared the dictionary between the caller's `d` and the
  parameter, so the helper's `Dict.insert` copied it and a loop of such calls was quadratic. A
  first version that cleared the caller's binding was removed, because a second argument naming
  `d`, a nested closure, `get(..., parent.frame())` and an error unwinding through the assignment
  all read the cleared variable.
- **Target shape** (`crates/interp/src/moves.rs`). `rebind_call` offers a closure call to
  `moves::call_moving`. A static check (`movable`) requires positional arguments without `...`,
  and that the other arguments, the callee's defaults and its body call only closures and inert
  builtins (`inert_builtin`, plus native-accepting library builtins), never naming the caller's
  variable unless it is the callee's own local. Then the caller's binding is set to `NULL` for
  the call. At run time any code the check did not cover ends the move first and restores the
  binding: entering a closure other than the callee, forcing a promise the check did not see, a
  library callback that is not a closure, and the default error handler.
- **Undo journal.** An in-place update of a dictionary or set either appends or replaces a value
  (`insert_in_place` reports replacements to `moves::replacing`), so the original is the first
  `entries` entries with the replaced values put back (`collection_prefix`). A failed call, or an
  early end, rebuilds the caller's value from it.
- **Dead frame release.** When the call returns without the move having ended and the callee
  creates no closure, its frame is unreachable, so `release_parameter` clears the parameter's
  binding. Without that the dead frame keeps the result shared and the next call copies. The
  heap does not count an environment's enclosure as a reference, so the environment's refcount
  cannot decide this the way GNU R's `R_CleanupEnvir` does.
- **Holes the static check cannot see.** A variable that holds an environment can reach any
  frame, so `$`, `[[` and their assignments on an environment end every move
  (`moves::reaching_environment`, in `subset.rs`). A local assignment of the caller's variable's
  name does not shadow it, since a read before the assignment, or on a path that skips it,
  finds the caller's variable, so such a body is not movable; only a parameter shadows. A closure
  created in the body captures the frame without entering a closure, so every live move records
  each closure created (`moves::created_closure`), and the frame release and the tail update
  both require that each is referenced only from the move's private frames (`closures_dead`).
- **A shared start does not stick.** A value something else also holds, such as `.Last.value`
  holding the dict a top-level `d <- Dict.empty()` created, cannot move, but the call still runs
  tracked (`Move::shared`, binding untouched) so the dead frame is released and the next call
  moves. Before this, one shared call made every later call copy, because the dead frame kept
  each result shared.
- **Tail update.** A call `f(p, ...)` in tail position, `f` a library builtin that reuses its
  first argument and `p` the first parameter, compiles to `Op::CallTailUpdate`
  (`bytecode::tail_updates`). Tail position is the body, the last statement of a `{` or a branch
  of an `if` in tail position, or the argument of a `return` in a statement outside any function
  literal or `local`. It updates `p`'s value in place only when, after its arguments are
  evaluated, a move into this frame still lasts and every closure created while it lasted is
  dead once the frame is, with none among the call's other arguments, whose values the heap does
  not count (`moves::frame_private`): then nothing outlives the frame, and nothing in it runs
  after the call, so no one sees the parameter change. A body that creates closures used to be
  refused statically; the dynamic rule turned 20,000 calls of `h <- function(d, k) { inc <-
  function(x) x + 1L; Dict.insert(d, k, inc(k)) }` from 30.6 s into 0.85 s (debug build). The AST evaluator never does this, so the first two calls, before tier 0 compiles the
  body, copy.
- **Why static types do not prove it.** A type says what a value is, not which frames a
  function reads or whether it captures its environment, so a typed helper needs the same body
  check as an untyped one. Effect annotations would be the typed route, and ry has none.
- **Measured.** 40,000 calls of `d <- add(d, i)`, profiling build: with `add <- function(d, k)
  { d <- Dict.insert(d, k, k); d }` inside a function, 16.9 s before and 0.10 s after; at top
  level inside `system.time`, 16 s before the shared-start fix and 0.066 s after; with the tail
  form `add <- function(d, k) Dict.insert(d, k, k)`, 16.4 s before and 0.053 s after.
- **Lists.** A plain list (no attributes, not an Array) moves like a dictionary: `moves::extent`
  gives its length as the journal's `entries`, and `List.set` and `List.swap` report replaced
  elements to `moves::replacing`, so `original` rebuilds it. Growth past capacity copies and
  leaves the original intact. 40,000 calls of `xs <- helper(xs, i)` with
  `helper <- function(xs, i) List.append(xs, i)` take 0.046 s on the profiling build.
- **Closure-creating helpers.** `movable` accepts a `function` literal whose defaults and body pass
  the same check (`check_literal`) and returns those bodies. `eval::make_closure` reports every
  closure to `moves::created_closure`, which marks it checked when its body is one of them, and
  entering a checked closure no longer ends the move; its frame joins the move's tracked frames,
  so forcing a promise made there is covered too. At return, `closures_dead` releases the
  parameter only when no created closure is the result and each one's refcount is covered by the
  tracked frames' own cells, which means none escaped. 40,000 calls of
  `{ inc <- function(x) x + 1; xs <- List.append(xs, inc(i)); xs }` take 0.14 s, and the tail
  form `{ inc <- ...; List.append(xs, inc(i)) }` scales linearly too (0.06, 0.14 and 0.26 s at
  20,000, 40,000 and 80,000 calls), although `Op::CallTailUpdate` stays off for a body that may
  capture its frame.
- **Base replacements end the move.** `xs[[i]] <- v`, `xs$a <- v`, `xs[i] <- v` and nested forms
  on a moved list modify it in place without reporting to the journal, so `subset::modifiable` and
  complex assignment in `eval.rs` call `moves::modifying` first, which ends a move whose value is
  the object being changed. It must run before the sharing check, because ending the move makes
  the object shared again and forces the copy. A copied closure (`values` duplicate) is reported
  to `created_closure` like a fresh one.
- **Slot frames.** A function's frame that keeps its slots has no binding cell to move from, so
  the move names its binding as `moves::Binding` (a cell, or a slot by the header's register and
  the slot's position) and the caller's names as `moves::Heads` (an environment, or a slot frame
  by index). `op_call_rebinding` in `bytecode.rs` hands a closure call to
  `moves::call_moving_from`, the same check, journal and restore that `call_moving` uses from an
  environment, with the other arguments built as the frame's lazy promises. `resolve_function`
  reads a slot frame's own bindings without making its environment, and the move is refused
  while a register of the calling expression holds the value, since registers are not counted.
  2e6 calls of `xs <- helper(xs, i)` inside a function take about 2 s for a list, a dictionary
  and a set alike, against 5.8 s for 40,000 before.
- **Limits.** The tail update covers only the last statement and stays off in a body that creates
  a closure. Both are in `backlog.md`.

## A key of the wrong type is an error, not a missing key

**Status:** built.

- A dictionary or set holds keys of one kind. A lookup with a key of another kind used to return
  `:Err(:KeyNotFound)`, which hid a bug the checker reports statically. It now stops with "the
  dict's keys are character, and this one is integer; a collection holds one type", through the
  same `joined_key_kind` check that insertion uses. Integer and double keys still join as numbers,
  and an empty collection has no kind yet, so any key misses.

## Records, tuples and tags compile natively

**Status:** built.

- **Previous shape.** `Op::Construct` was an opaque step in the JIT (a helper call that re-read registers through the activation), a guarded `match` fell back to the `match` special for the whole expression, and `l[[i]]` on a plain list returned a boxed element, so arithmetic on it ran in tier 0.
- **Construct.** `Action::Construct` writes `(kind, bits)` pairs into a stack buffer using `runtime::representation`'s encoding and calls `runtime::jit_construct`, which builds the slots and allocates through `Heap::alloc_record_slots`. Unboxed doubles, integers and logicals go straight into unboxed slots (`Slot::Real`, `Slot::Integer`, `Slot::Logical`); a double whose upper word collides with an integer or logical prefix stays a handle, and slots past `UNBOXED_SLOTS` are boxed in codegen.
- **Guarded match.** The guard compiles in place with the arm's bound names read from registers (`Compiler::bound`), and `Op::MatchCondition` applies the special's condition rules (a record gets the boundary error). Anything that would reach the frame by name evaluates in a scratch child frame holding the bindings, through `Op::BoundEval`: a call the compiler cannot inline (a closure's promises would read the frame), `[[`-style subject dispatch, and every runtime fallback. A guard that assigns, creates a closure, nests a `match`, leaves a loop or calls a bound name (`guard_compiles_in_place`) runs whole through `Op::BoundEval`, which keeps the special's semantics: an assignment in a guard never reaches the function's frame. The JIT turns `MatchCondition` on a logical into a move, so a guard of plain arithmetic and comparisons runs natively.
- **`MatchFail`** always signals an error, so the JIT treats it as `PureUnlessObserved` and leaves every register `Bottom` after it. As an opaque step that fell through with its registers' stale types, it widened the join after each `match` (a reused temporary still typed as the scrutinee's record) and left the loop's arithmetic in tier 0.
- **List element reads** speculate on the site's type feedback exactly like tuple items and record fields: a check on the loaded element hands off to tier 0 on a mismatch.
- **`List.range` and `List.repeat`** stop with an error past 2,147,483,647 elements (R's longest integer-indexable vector), so a typo cannot ask for an unbounded allocation.
- **Benchmark.** In `tools/bench`, `#@ typed` / `#@ untyped` variant blocks ("The bench templates are inline-typed") let the finance `ledger`, data-science `readings` and biostatistics `followup` templates use tags, tuples and records when typed and `list(kind=)` when untyped, and both draw the same instances.
- All three benchmark code bases use records, tags and tuples in their typed program (finance `ledger`, data-science `readings`, biostatistics `followup`). The typed folds run in 0.05 to 0.10 s, against 0.65 to 0.91 s untyped and 0.40 to 0.70 s in GNU R.

## The standard library is the `std` namespace, with an optional prelude

**Status:** built.

- **Previous shape:** the library's signatures lived in `types/ry.Rtypes` beside the DataFrame API, in
  the always-active namespace `ry`, so every name was always in scope and `ry::List.map` was the only
  qualified spelling. Its per-function reference was a hand-written copy of the stub docs in
  `reference/standard-library.mdx`, which drifted from the declarations.
- **Prior art:** Roc auto-imports its builtin modules with no opt-out; Rust has `std::prelude` and
  `#![no_implicit_prelude]`; Gleam has no prelude and imports explicitly. R already reads `pkg::name`
  as namespace access, so the Rust shape fits without new syntax.
- **Chosen shape:** the library is its own namespace, `std` (`types/std.Rtypes`, `types/std.exports`;
  `ry` keeps the DataFrame API and Arrays). `std::Name` always resolves. `[check] prelude`, default
  `true`, additionally puts the bare names in scope; `false` makes a bare name unbound (with a fix to
  `std::Name`) unless the project defines it. Types (`Result`, `Dict`, `Set`, `Array`) stay in scope,
  because they are type syntax, not namespace members. The checker gates bare lookups through
  `stubs::outside_prelude`, which reads the setting only for library names, so flipping it rechecks
  only items that use the library (`test_incremental.rs`). The runtime binds both namespaces and
  attaches `package:std` only with the prelude on; the host passes the setting as the negative flag
  `RUN_FLAG_NO_PRELUDE`, so `R CMD` and older hosts keep the prelude.
- **Docs:** `crates/semantics/tests/test_std_docs.rs` renders `docs/.../reference/std/` from
  `types/std.Rtypes` and fails when the committed pages are stale (`just docs-std` rewrites them), so the
  declarations are the single source of truth for the checker, hover and the docs.
- **Impact:** one source of truth for the API docs; no new syntax; incremental cost bounded to items
  that use the library. Gap: compiled code fast-paths only bare library calls, so `std::List.map`
  is not fast-pathed or JIT-looped (`backlog.md`).

## Typed closures run as native functions over records, tags and match

**Status:** built.

- **Previous shape.** A typed closure ran as any other: tier 0 matched arguments, built promises
  and an environment, and tier 1 compiled the body once a profile was hot, speculating on boxed
  `SEXP` values. Natives (`crates/jit/src/native.rs`) existed only for scalar and vector bodies
  reached from compiled code, and any record, tuple, tag or `match` refused them. Typed programs
  ran no faster than untyped ones (orders 0.43 s typed vs 0.50 s untyped).
- **Target shape.**
  - *Call-time entry.* `Tier.call` (`jit_call` in `runtime.rs`, called from `apply_closure`) runs
    a typed closure (`annotations::is_typed`) as a native at its first call. Arguments must be
    positional or name-exact, and each value is read without forcing: a constant, a forced
    promise, a promise of a constant, or a promise of a symbol bound to a plain value. Its `Ty`
    comes from the value (scalar, attribute-free vector, plain record as `Rec(layout)`), the
    native is cached per closure and `Ty` list, and a trampoline cached per machine signature
    passes words in and boxes the result. Debug, step, trace and moves tracking skip the path.
    This supersedes the `BRIEF` refusal above for typed closures reached this way.
  - *Records as pointers.* `Ty::Rec(layout)` holds a record pointer; distinct layouts join to
    `Ty::Obj`. Field reads speculate from per-layout slot-kind profiles that the heap records at
    allocation (`heap::records::slot_kind`) and native construction records at compile time; the
    read checks the unboxed bit and the integer/logical prefix, an empty profile makes the read
    fail, and a mixed profile refuses the native.
  - *Construction and match.* A record is built from a stack array of (representation, bits)
    pairs by `jit_native_record`. Zero-slot tags are shared immutable singletons. `match` with
    exact-layout patterns, `_` and binds resolves statically on `Rec(L)` and branches on the
    layout id for `Obj`; a non-exhaustive match fails to tier 0. Guards, `|`, `..`, literals and
    nested patterns refuse.
  - *Rooting.* `jit_native_mark`/`jit_native_release` bracket entry, return, failure and loop
    back edges, except where a loop carries a record into a pre-loop local.
- **Why not more.** Annotation checks stay storage-only, and a native fails (tier 0 reruns the
  call) rather than deoptimizing mid-body, which keeps every native side-effect-free until it
  returns. Uniqueness in-place update, scalar replacement of non-escaping records, strings and
  the standard library inside natives, vector builtins, and recompiling when a profile widens
  are not built (`backlog.md`).
- **Impact.** orders 0.50 s (GNU R) / 0.44 s (untyped ry) / 0.034 s (typed), expr_tree 0.33 / 0.34
  / 0.008 s, checksums equal. `fuzz_typed_record_natives_agree_with_tier_zero` in
  `crates/jit/tests/test_tiers.rs` pins agreement across JIT modes.

## Typed ry has its own numbers, `Bool`, `Str` and `List`

**Status:** built, except the closing note that typed `I64` code runs in the interpreter, which "Typed `I64` runs in registers, and list arithmetic fuses into one loop" supersedes.

- **Before:** typed ry borrowed R's `integer`, `double`, `logical` and `character`, which carry `NA` and recycle, and `Array<T>` was a strict R vector. Typed programs could not be written without R data types.
- **Shape:** two number types, `I64` and `F64`, plus `Bool`, `Str` and `List<T>` (which replaces `Array<T>`), all `NA`-free. The design is `development/design/native-types.md`; the contract is the reference section "Numbers, `Bool` and `Str`".
- **Number set cut to `I64` and `F64` (orchestrator decision).** The first version took all thirteen of Roc's number types (`I8`..`U128`, `F32`, `Dec`). Each width multiplied the checker's constraint cases, the interpreter's arithmetic and the JIT's register types, and no typed program used anything but `I64` and `F64`. Another width returns only when the benchmark hill climb shows a program that needs it. Any other literal suffix is a syntax error (`syntax` lexer). The internal literal constraints (`Constraint::Int`, `Constraint::Frac`) default to `I64` and `F64` and have no annotation spelling; `number` is the only written one. An R `integer` or `raw` value becomes `I64` at the boundary.
- **Representation:** `F64`, `Bool` and `Str` reuse R's `double`, `logical` and `character` storage, so they cross into R for free. `I64` has its own vector type, `SexpType::I64` (code 28, `I64SXP`), with `REALSXP`'s layout: the 16-byte header, length and capacity, then 8 bytes per item at `VECTOR_DATA_OFFSET`. A scalar `I64` is a length-1 vector and a `List<I64>` is the same object at length n with the heap's Array flag (`numbers::alloc`, `numbers::list`). It carries no attributes and is never ALTREP.
- **Why a dense `I64` vector (decision).** The previous shape was a boxed record per number (`Fields::Number`, one raw slot) and an R list of boxes for `List<I64>`, which gave two representations for one type, one GC-traced object per item, and an item-by-item arithmetic path (`arith::item_by_item`, `Items::RyValues`). A dense vector gives one representation for scalars and lists, a zero-copy handoff to the typed runtime (`crates/typed`), no per-item tracing (it is a leaf), vectorized same-length list arithmetic (`numbers::arithmetic`, no recycling, overflow an error) and storage compatible with bit64's `integer64`. A new type code beats a gp bit on `REALSXP`: R code and C code that test `TYPEOF` refuse it instead of reading the bits as doubles, and `is_atomic` is false for it, so every R builtin raises the `Num.to_r()` boundary error (`records::boundary_message`). Crossing to R stays explicit through `Num.to_r` and `List.to_r`.
- **Literals:** a suffix names the type (`5i64`), and a `-` written directly before a literal is part of it. An unsuffixed literal keeps R's type unless an expected type, the other operand, or a later use decides it. A literal branch adapts to the sibling branch's ry type (`adapt_literal_branch`), and a match arm or `if` arm in a function's tail adapts to the declared result.
- **The checker decides a literal's run-time type (decision).** Before, the runtime converted literals by its own syntactic rules (the operator in `numbers::operands`, the annotated binding in `lower::with_annotated_literals`), so a literal the checker typed `I64` anywhere else stayed an R `double`: a `match` arm adapted to a declared `I64` result returned a `double`, which broke soundness, and so did an unannotated `[1, 2]`. Now each `ItemCheck` records the unsuffixed literals it typed `I64` (`i64_literals`), `semantics::file_elaborations` maps them to file ranges, and the preflight elaborates the program for ry's runtime by spelling each as `5i64` (`preflight::elaborate`), the mirror of erasure for GNU R. Empty lists take the same route: an empty list is stored by its items, so one built from no items (`[]`, `List.map` over nothing) is `logical(0)` and `List.sum` of it was R's `integer` 0. `ItemCheck::typed_empties` records `[]` and each call of a library function whose result `List<B>` gets `B` from no list argument (`check::builds_fresh_items`; `List.map` qualifies, `List.keep_if` does not), with the resolved item storage, and the preflight wraps each as `.ry_items(expr, "I64")`, which returns its argument unless it is an empty list of other storage. The decision is the checker's, not a run-time guess; the wrapper is a no-op on every other value, so a shadowing project function is safe, and the typed core lowers it as its argument. `List.keep_if` needs no wrapper because the runtime builds a result drawn from its input's items (`keep_if`, `drop_if`, `take_first`, `sublist`, `sort_with` and the other slices) through `standard_library::subset`, which gives an empty result its input's storage. An operator on an `I64` list treats an empty list of any storage as an empty operand (`numbers::operands`), so a typed empty list meets an unannotated `[]`. The wrapper shows in code the program reads back as data (`match.call`, `body`, `substitute`, a `stopifnot` message), an accepted cost like the `5i64` spelling. `ry run` hands the elaborated text to the runtime through `ry_set_script_source`, and the console's filter releases it as the command. A file with no ry syntax and no strict mode skips analysis (`preflight::has_ry_syntax`, which also counts `#:` comments and `Module.function` calls). Columns after an elaborated literal shift right in run-time error messages, an accepted cost. A `List<I64>` given to `DataFrame()` becomes an R `integer` column when every item fits and `double` otherwise, as `Num.to_r` converts (`frame::column_contents`), because an elaborated `[3L, 5L]` column is an `I64` list. `with_annotated_literals` remains because `ry load` and `source()` run unelaborated text; it is redundant on elaborated text, and goes once those paths elaborate too. `ry run` rewrites the file's own bytes, not a lossy UTF-8 copy: the check reads each invalid byte as `?` so offsets stay byte offsets, and `ry_set_script_source` escapes invalid bytes as reading the file does, so a Latin-1 script keeps its bytes. A whole literal spelled with a fraction or exponent (`2.0`, `1e3`) converts digit by digit, because an `f64` rounds past 2^53.
- **String and logical literal variables (decision).** `greeting <- "hi"` and `flag <- TRUE` bind a literal variable (`Constraint::StrLiteral`, `BoolLiteral`) the way a number literal binds `WholeLiteral`/`FracLiteral`: an annotated binding or a typed parameter in the same item decides `Str`/`Bool`, and every other read defaults it to R's type, so plain R types exactly as before. Meeting a `List` item variable binds the ry type directly in the var-var arm of `unify` (`infer.rs`), because no constraint spells "only `Str`". Call arguments and annotated-binding values that are plain reads join `literal_reads` so the call or annotation, not the read, decides them.
- **Top-level literals in typed files are ry values (decision).** Previous state: each top-level statement is its own item (the per-item firewall), so a top-level `rate <- 2.0` could never be decided by a later statement and defaulted to R's `double`, which made every typed script start with R values and rejected `half(rate)` for `half(x: F64)`. Rejected alternatives: letting later items decide a top-level literal breaks the firewall (an edit to any reader would re-type the binding and every other reader); keeping R types forces suffixes or annotations on every constant. Chosen shape: in a strict file or a file with ry syntax (`semantics::file_holds_ry_values`: configured strictness, or an inline-annotation or ry-syntax site), a literal bound outside any function (`Checker::binds_ry_value`, `return_frames` empty) is a native literal variable, so it defaults to `I64`/`F64`/`Str`/`Bool` (`5L` included, an `I64`). A value of literals alone (`literal_value_scalar`: arithmetic, logic and comparisons, an `if` of such branches, a one-statement `{}`) is one literal everywhere a lone literal adapts: top-level bindings, expected types, `List` items, `number`-bounded parameters, and beside a `Bool` operand of `&&`/`||`. A number literal written with `.` or an exponent and no `L`, or a whole one past `I64`'s range (`NumberLiteral::default_native`), defaults to `F64` here and in `List` items, as Roc's `2.0` is a `Frac`. The boundary stays explicit: an `I64`, or a `List` whose items share no R representation, passed to an R function is `NativeValueToR` naming `Num.to_r()`/`List.to_r()`, the static twin of the runtime's boundary error; `F64`/`Bool`/`Str` and lists of them share R's representation and pass unconverted, because no conversion happens. Plain R files keep R types. A host with no settings counts as strict for strict origins but not here, because this changes types. Impact: the firewall is untouched (the flag is one tracked bool per file); typed scripts need no suffixes on constants.
- **`/` decides `F64`** for undecided items and is a `type-mismatch` on `I64`, as Roc gives `/` a `Frac`.
- **Lengths, counts and positions are `I64`** (`standard_library::whole_number`), as in Roc.
- **List operators work item by item** and never recycle: on a `List<I64>` they run over the buffers (`numbers::arithmetic`), and on nested lists item by item (`arith::item_by_item`).
- **`try f(x)`** is the `Ok` value or an early return of the `Err`; `try (expr)` keeps R's `try()`.
- **No row variables.** An open tag union cannot pass its unlisted tags through with their types; this stays until a real program needs it (user direction to drop them from the design).
- **Impact:** typed programs are written in ry types only (`tools/bench/kernels/*.typed.R`, `tools/bench/applications/*.typed.R`). The cost is speed: the JIT has no register type for `I64`, so typed `I64` code runs in the interpreter (`backlog.md`).

## Typed values are native, immutable, acyclic and freed by reference counts

**Status:** built, except the per-type struct-of-arrays pools with 32-bit references, which "Typed boxes have shapes, and a tag type is a union of them" supersedes.

- **Previous shape.** Typed natives (`crates/jit/src/native.rs`) build records as R heap objects
  traced by the mark-sweep collector and rooted through handle marks, and the first draft of
  `development/design/typed-runtime.md` kept typed lists and strings as R vectors and `CHARSXP`s
  with stack maps for roots.
- **Chosen shape.** Typed ry's own values (numbers, `Str`, `List`, `Dict`, `Set`, records, tuples,
  tags, closures) are native values outside the R heap. They are immutable and acyclic (a closure
  never captures itself; recursion goes through a lifted definition), so compiler-inserted
  reference counts in the Perceus style free them deterministically, with borrow inference as in
  Lean 4, field moves, drop specialization and reuse of a uniquely held block. No tracing collector
  sees them, so compiled frames need no stack maps. R values stay under the collector and meet
  typed code only at the boundary: an entry stub checks a guess and runs a second compiled copy
  that reads the R value in place, or the program converts explicitly (`List.from_r`).
  References go one way: a native block may preserve an R value, but every native value crossing
  to R is converted (a typed closure becomes an R closure over converted captures), so no cycle
  runs through both heaps. R values in compiled locals sit on the handle stack.
- **Why.** Exact counts make in-place update a count-of-one test, freeing deterministic, and a leak
  checkable at the end of every test run. Native layouts (inline small strings, unboxed lists of
  flattened records, a dense-entry `Dict`) are the Rust-shaped data the 1.5 to 2 times Rust bar
  needs, and R vector headers, intern tables and collector tracing would cost on every operation.
- **Layouts.** Data-oriented throughout: a heap record or tag variant is a slot in a per-type pool
  stored as a struct of arrays (one column per field and one count column), a reference is a
  32-bit index (28 index bits, a closed union's 4-bit variant index above them; open or wider
  unions use 64-bit references), with column bases reloaded after any call that may grow them, lists store record items as columns and `Bool`
  as a bitset, short non-escaping lists live on the stack, and a `Dict` over densely numbered keys
  is a flat array plus a presence bitset, while a hashed `Dict` packs seven hash bits into the upper
  bits of each index entry.
- **Impact.** A `List<F64>` reaches C only through a copy, and an ALTREP wrapper is the deferred
  fix if the boundary copy shows up. The design and its test matrix are in
  `development/design/typed-runtime.md`.

## Typed code compiles ahead of time, and `ry build --aot --exe` writes an executable

**Status:** built. `ry run --aot` compiles a script's typed functions before it starts (the entry "`ry run --aot` compiles a script's typed code before it starts"), `ry install` writes an installed package's typed code file (the entry "The typed code file is position-independent code whose offset table is the per-process table"), `ry build --aot --exe` writes an executable (the entry "`ry build --aot --exe` appends the script and its code file to a copy of `ry`"), and plain `ry build` writes a source tarball.

- **Command line (user directive).** Compiling ahead of time is asked for with `--aot`: `ry run --aot main.R` compiles before the program starts, so nothing compiles while it runs; `ry build --aot` on a package compiles into the installed package, and `ry install` does the same; `ry build --aot --exe main.R` writes one executable and comes after the up-front path, because compiling up front matters more than a single file. Without `--aot` typed code compiles at its first call.

- **Chosen shape.** `ry run` and `ry load` compile each typed function of a strict file before its
  first call (when the lazy loader forces its binding, with workers compiling its callees ahead),
  and `ry install` compiles all of a package's at install time into the
  installed package, so typed code never starts in an interpreter. There is no code cache
  (AGENTS.md, "Performance principles"): an installed package's code file is build output, one
  position-independent file per namespace, mapped read-only and shared between processes, with a
  per-process table for addresses, a stamp of the ry build and of the package database, and every name the code resolved checked again when a function attaches (the entry "The typed code file is position-independent code whose offset table is the per-process table"), and code
  for a script or a package under development is compiled in memory as the session uses it. `ry build --aot --exe` appends that file and the
  elaborated script to ry's runtime as a standalone executable. Plain `ry build` keeps writing a
  source tarball, as `devtools::build()` does, because existing workflows rely on it.
- **Tests.** Every typed program runs interpreted, compiled, and compiled with annotations erased,
  with identical output required, plus `RY_FORCE_GUESS_FAIL`, `RY_CHECK_FACTS` and an empty-pool
  leak check (from Bun's compiler, `prior-art.md`). `ry check --explain` reports slow paths worst
  first by loop depth.

## `ry run --aot` compiles a script's typed code before it starts

**Status:** built for a script's top-level definitions and for files it sources into the global environment. The package code file is its own entry ("The typed code file is position-independent code whose offset table is the per-process table"). Not built: the executable. Decided against: packages under `ry load` (below).

- **Previous shape.** The typed core compiled a strict typed closure at its first call, keyed by the closure and the argument types that arrived (`COMPILED` in `crates/typed/src/typed.rs`). Three things existed only once the program ran, so nothing could compile earlier: the closure itself (made when its `name <- function` assignment runs), the type a `List.from_r` or `Num.from_r` gives (learned by letting the interpreter run the conversion once and recording what came back), and a function a namespace binds lazily (a promise the interpreter forced at the first use).
- **What was weak.** The observed conversion type was a second source of truth beside the checker, which had already typed the same expression, and it cost every converting function one interpreted call before it could compile. On the typed benchmark programs the compile ran inside the measured region: 1 to 6 ms per kernel or application, and 51 ms for `pidigits`.
- **Target shape.** `ry run --aot` sets a run flag (`RUN_FLAG_AHEAD`). `run_script` then lowers every top-level expression before the first one runs, creates the closure of each top-level `name <- function` definition (`crates/interp/src/ahead.rs`), and hands the list to the tier through `Tier.ahead`. The typed core (`crates/typed/src/ahead.rs`) compiles each definition whose declared parameter types are types R can pass (numbers, `Bool`, `Str`, lists of those, tag unions, tables with or without a column list), and its typed callees with it. The pass reads the parameter types from the annotation text (`annotated_type`), because the runtime holds annotations as text and storage masks; the preflight writes each one-line annotation in canonical spelling, so the text is the checker's type (the entry "An installed typed package holds elaborated sources with canonical annotations").
  - **Sibling calls.** A name the script has not bound yet resolves to the closure created up front (`DEFINED_LATER`, the first definition of each name). The compiled call carries the same speculation as any global call, a function identity checked on every call (`still_resolves`), and the assignment hands out the closure created up front (`ahead::take`), so the check holds once the script has run the definition. A name bound to another function later fails the check and the caller compiles again, as it does without the flag.
  - **Conversions have the checker's type.** The preflight wraps a `List.from_r(x)` whose item type the checker knows as `.ry_items(List.from_r(x), "F64")` (`"I64"`, `"F64"`, `"Str"`, `"list"`), and the typed lowering takes a scalar name as the conversion's type (`Conversion.checked`), ahead of the type the interpreter saw the conversion give. One function, `arrays::checked_conversion`, turns what R gave into a list of the checked type in the interpreter (`do_items`) and in compiled code (`write_converted`):
    - The checker's R types are coarser than the vectors R computes. `abs(-3:3)`, `sample(5)` and a `double[]` parameter passed `1:3` are typed `double` and hold integers, in programs `ry check` accepts. So the conversion widens as R coerces: `Bool` or `I64` items become `F64` where the checked type is `List<F64>`, `Bool` items become `I64` where it is `List<I64>`, and an empty vector becomes an empty list of the checked type.
    - Any other difference (`F64` items where the checker said `I64`, strings where it said numbers) is an error with one message in both tiers. It means R code gave a value its types do not describe, for example after `assign()` rebound the function behind `x`, or where an R type rule of the checker is wrong.
    - Rejected: an error on every difference in storage. It was built first, and it stopped correct programs, because of the coarse R types above. Also rejected: compiled code falling back and compiling again for the type that arrived, a second code path whose only users are programs the checker mistyped.
  - **Lazy bindings.** A function that a loaded namespace binds lazily is forced when typed code compiles, in every mode. Forcing reads the package database and runs no user code. A namespace that is not loaded stays a wait, because loading it runs its `.onLoad`.
  - **Refusals are retried.** A refusal met before the script ran may only say that the script had not run yet, so refusals are dropped after the pass and the first call compiles again.
- **What still compiles at a first call.** A `Num.from_r` or `List.from_r` the checker cannot type (a guess, which a failed guess replaces with the interpreter; see "Typed code calls R natively"), a call into a namespace not loaded yet, a function defined in a block or in a file sourced into another environment, a definition in a statement whose lowering warns, and a top-level function with a record, tuple, `Dict`, `Set` or function parameter that no compiled-ahead function calls.
  - **Sourced files (decision).** `parse()` of a whole file under `--aot` (`lower::do_parse` calling `ahead::parsed_file`) makes the closures of the file's typed top-level definitions and hands them to `Tier.ahead`, the script's own entry, before `source()` evaluates any of it. Only a definition evaluated in the global environment takes its closure (`ahead::take`), so the file compiles only when the frame calling `parse()` binds `envir` itself to the global environment, read without forcing anything (`eval::peek_frame_variable`, which does not look in enclosing environments, so a global variable named `envir` decides nothing): `source()` at the top level. `sys.source()` calls the R-level `parse()`, whose frame binds no `envir`, so its file compiles at first calls. Rejected: compiling every file `parse()` reads, which compiled `sys.source(file, envir = e)` and `source(local = TRUE)` inside a function for closures nobody binds. Rejected: finding `source("...")` calls in the script up front, which misses computed paths and nested sources and would read files before the script could create them.
  - **`ry load` (decision: not compiled ahead).** `ry load` defines a development package's functions lazily, on the first use of each, which is its point (the loader entries in this file). Compiling all of a package's typed functions up front would force every definition the session never uses. A function `ry load` defines compiles at its first call, which comes right after the definition, so there is no earlier moment to move the compile to. Install the package to get its typed code file.
  - **Namespaces not loaded yet (decision: wait for the first call).** Loading a namespace up front so a caller of `pkg::name` could compile would run the package's `.onLoad` before the script's first statement, which changes the order of its output, options and errors. The cost of waiting is one interpreted call of each such caller, and no typed benchmark program calls a package namespace (they are written in ry's own types). Compiling the caller when the namespace loads would save that one call; build it when a measured program shows the call.
- **What the pass must not change.** A script prints the same, and signals the same conditions at the same points, with the flag and without.
  - Lowering signals a warning for a literal such as `1.5L`. Lowered up front, the warning would arrive before a handler or `options(warn = 2)` that the script sets earlier. An expression whose lowering warns or fails is therefore left to the run, which lowers it at its turn (`ahead::holds_back_warning`).
  - The session's limit on compiled closures (`MAX_COMPILED`) bounds closures a running program makes. A script's definitions are bounded by its text, so the functions compiled before the script starts are not counted (`ahead::compiled_ahead`).
  - The session holds each lowered expression until the run takes it. The run roots it in its own handle scope for the statement, because nothing else holds a statement while it runs: without the root, a collection between two arguments of a statement frees the later ones (`ahead::lowered`, pinned by `run-aot.scenario`). The closures of definitions that never ran are released when the script ends.
- **Measured (iterate build, one machine).** All 70 typed benchmark programs (11 kernels, 6 applications, 53 public) print what the first-call run prints and compile nothing after the script starts. The up-front compile takes 1.1 to 5.9 ms on kernels and applications. A program's first run of its measured function gets shorter by the compile it no longer holds: `orders` 3.50 to 1.27 ms, `report` 11.88 to 4.66 ms, `team_scores` 9.45 to 3.97 ms, `log_rank` 34.56 to 22.02 ms, `pidigits` 51 to 2 ms. Warm times and whole-process wall times are equal within noise, because the same compiler does the same work earlier. The checked `List.from_r` type and the forced lazy bindings apply without the flag too: `vasicek`'s first run fell from about 140 ms to 74 ms.
- **Impact.** Correctness: a checked conversion gives one list or one error in every tier; every mode runs the same code, so the differential matrix (`typed_scripts_agree_however_they_compile`: interpreted, first call, ahead, ahead with every sibling guess failing under `RY_AHEAD_MISS=1`, ahead with tier 1 forced, annotations erased) holds with identical output. Simplicity: no new code store, since code compiled ahead lives in the table first-call code lives in. Performance: the wall time of a process is unchanged; latency moves from the first calls to before the program. Incremental analysis: untouched, the checker hands one more fact through the preflight.
- **Pinned by** `run-aot.scenario`, `typed-code-calls-r.scenario`, `typed-table-values.scenario`, and in `crates/ry/tests/test_scenarios.rs` `typed_benchmarks_run_natively`, `typed_scripts_agree_however_they_compile` and `a_script_with_more_definitions_than_the_closure_limit_compiles_them_all_ahead`.

## An installed typed package holds elaborated sources with canonical annotations

- **Previous shape.** `ry install` and `ry build` built the tarball from the package's own text, which the preflight never saw. So an installed typed function had no checked conversion type (`.ry_items`), its literals ran as R doubles where the checker said `I64`, and an alias in a signature could not be read: `annotated_type` (`crates/typed/src/ahead.rs`) followed names through `Script::named_types`, a second reader of type definitions that the lowering built from a script's text and that no namespace supplied, and the run-time mask reader reads an alias as unchecked.
- **What was weak.** Two sources of truth for named types (the checker's `TypeDefinitions` and the lowering's text scan), and installed code that compiled less than the same code as a script. Before this change, of 70 packages generated from the typed benchmark programs (`tools/bench/typed_package.py`), 19 failed to install (lazy loading met an `I64` the serializer refused) and 17 ran a function in the interpreter before or instead of compiling it.
- **Target shape.**
  - `preflight::elaborate_package` runs the check over a typed package (strict in `ry.toml`, or a file with ry syntax or `# typing: strict`), refuses the build on a strict error, and returns the elaborated text of each file, encoded back into the package's declared encoding. A rewrite that changes the line count or the parse errors is a bug in ry and fails the build rather than installing code the checker never saw. `build_tarball` (`crates/ry/src/main.rs`) copies the package into a `tempfile` directory (skipping `.git` and `.Rbuildignore` matches, which `R CMD build` applies again, and entering each directory once so a link cannot loop), writes the elaborated files there, and builds the copy, so the package directory is never touched.
  - In every file the preflight elaborates (a script under `ry run`, a console command, a typed package's files; `ry load` and `source()` run unelaborated text), each one-line parameter and return annotation is rewritten to the checker's type in one canonical spelling (`semantics::canonical_annotation`): aliases expanded and every nominal type of the project unfolded to its representation, except a recursive one inside its own representation, printed by `TypeRenderer`. A spelling is used only when reading it back, as a parameter's and as a result's type, gives the same type, so a rewrite never changes what the checker declared, and unfolding stops after a fixed budget of visited types (`UNFOLD_BUDGET`), because definitions that each name another twice double the spelling at every step; an open record, a type parameter or an unknown name stays as written, and so does a spelling with a non-ASCII field name. `annotated_type` reads this closed grammar, and `Script::named_types` is gone. Elaboration keeps line counts, so source references hold.
  - Any `I64` or `List<I64>` serializes as ry's own type code 28: an array flag, the length, then each item's high and low 32-bit halves (`crates/libr/src/serialize.rs`, read back in `crates/interp/src/serialize.rs`). `lazyLoadDBinsertValue` accepts native values, so a typed package's `I64` constants reach the database; other builtins such as `saveRDS` still refuse a top-level one at the boundary. Records, tuples, tags, dictionaries and sets have no stream representation and are refused.
- **Behavior change.** In an installed typed package and a `ry run` script, a parameter typed by a record alias is checked at run time as the record, where the alias read as unchecked.
- **Stop rule count.** Of the 514 one-line types the signatures of the 70 typed benchmark programs write, 95 are rewritten and none is left as written for lack of a canonical spelling (`benchmark_signatures_all_have_canonical_spellings` in `crates/typed/tests/test_annotations.rs`, which also pins printer against reader over aliases, generics, tuples, records, functions, tag unions, nominal types, nested and recursive, and tables).
- **Measured (debug build, `RY_TYPED_LOG=1`, `RY_BENCH_REPEAT=0`).** All 70 generated packages install, print what the script prints, and run no function in the interpreter apart from the harness's R-boundary `measure`. Each package compiles at first calls exactly what the same program compiles at first calls as a script: 103 compilations in total, equal in every program. `ry run --aot` compiles 156 up front for the same programs, because it compiles every definition as its own root, including the small functions a first-call compile inlines into their callers and never compiles alone; the package's code file (backlog, step 2) is where that count applies.
- **Impact.** Correctness: an installed typed package runs the checker's types, as a script does, and the tier matrix (`typed_scripts_agree_however_they_compile`) runs each script's definitions as an installed package too. Simplicity: one source of truth for named types. Performance: no interpreted first calls in installed typed code; `ry install` of a typed package adds one check of the package. Incremental analysis: untouched; the preflight reads `file_elaborations`, `canonical_annotation`, `TypeDefinitions`, `InferenceTable::resolve`, `apply_definition`, `TypeRenderer` and `annotations::lower_written_type`.

## The typed code file is position-independent code whose offset table is the per-process table

**Status:** built for installed packages and for executables. `development/typed-code-file.md` is the contract.

- **Previous shape.** A typed closure of an installed package compiles in every process at its first call, about 0.8 ms each, and the code lives in that process's JIT memory. The backend writes process addresses into the code as immediates (runtime helpers, typed callees, R call sites, static strings and boxes, shape ids), so the bytes mean nothing in another process.
- **What was weak.** The same code is compiled again by every process and held privately by each, which is the cost the four test processes of a CI machine pay per dependency. A prototype of install-time code that is not in the tree measured 50 typed functions of a generated package on an iterate build: first calls took about 1 ms against about 40 ms, and added 1.8 MiB of peak resident memory against 5.9 MiB.
- **Target shape.** One file per namespace, `R/<package>.<build>.ryt`, written by `ry install` and `ry build --aot` in the session that writes the bytecode image. The code is position-independent: each process word is loaded from a table that sits at a fixed distance before the code, through the global-offset-table load Cranelift emits for an external name on a PIC ISA, and calls inside the file are direct relative calls. The writer resolves every relocation, so the code pages are never written and every process maps the same pages; only the table is private. A closure is named by its lazy-load record's offset in the `.rdb` and its position in that record, as in the bytecode image, and the header carries the build identity, the `.rdb` stamp and a target hash. A function attaches at its first call: the loader resolves the names it resolved at install and requires the class lowering specialized on (a function of the file, a base function, a table function, a standard library constant, a typed function elsewhere, any R function), builds its R call sites, attaches the functions it calls, confirms the shapes it constructs, and inserts the same `Compiled` entry a session compile makes. A version that does not attach compiles in the session.
- **Every process value is a table word.** Helpers and libcalls, the interrupt flag, shape ids, static boxes, the file's own read-only data (a PIC ISA sends every external name through the offset table, so data cannot be addressed relative to the instruction), R call sites and typed callees in other namespaces. Shape ids need care: lowering writes them as plain integers in a `match` and in the sort comparator, so the IR gets a shape constant, and types in the file name shapes by file index.
- **A version belongs to one closure.** The loader notes (record, position, database stamp) per closure object when the package database yields it, and only a noted closure attaches. A copy made at run time (an R6 method, `environment<-`, a per-class S4 method) shares the body and has another environment, which a version's R call sites hold, so it compiles in the session. The stamp in the note, not the database on disk, is what the file's header is checked against, so a reinstall in a running session cannot attach new code to closures of the old database.
- **Install elaborates first.** `ry install` runs the preflight and installs elaborated sources with each parameter annotation rewritten to the checker's type in one canonical spelling. Without it an installed function has no checked conversion types and no way to follow a named type, so the file would cover less than `ry run --aot` covers for the same code. This is the step before the file in the backlog.
- **Rejected.** Patching addresses into code pages at load, because a patched page is private to its process. A table pointer in a pinned register, because it costs every typed function a register and every entry a write. One emission mode for session and file code, because the session's code memory is the JIT module's and is not at a fixed distance from a table; the two targets differ in one `Slot` seam of the backend, and the tier matrix compares them.
- **What the file holds.** A version per declared root and its typed callees, plus, with `--train`, the versions the training workload compiled, after Leyden's stored profiles (`prior-art.md`). Untyped R is not in it ("Untyped R code is not compiled to machine code ahead of time").
- **No cache.** The file is build output, replaced only by reinstalling. A stamp that does not match sends the namespace through session compiles with a line that says to reinstall; nothing is rewritten at run time.
- **Trust.** The file is machine code and is trusted as a package's shared objects are. Hashes and stamps catch truncation, another build and a changed database.
- **Expected impact.** Correctness: file code and session code come from one lowering and one IR, and the tier matrix runs each program on both. Simplicity: one seam in the backend and one new module kind in `crates/ssa`. Performance: first calls at attach cost (target 20 µs against 0.8 ms), code pages shared between processes. Incremental analysis: none, the file is runtime build output and never a salsa input.
- **Measured** (iterate build, one machine). 50 typed functions of one package: first calls 1.43 ms from the file against 18.5 ms compiled in the session, about 21 µs per attach including the closure's lazy-load fetch; peak memory over `library()` +2.2 MiB against +5.0 MiB; `library()` 7.13 to 7.27 ms with a file against 7.51 ms without; code pages `Shared_Clean` across two processes. Warm time of file code over session code on the same installed package (15 rounds): orders 1.010, report 1.014, sales 0.975, drawdown 1.006, vasicek 0.996, so the table loads cost nothing measurable and the `Data`-slot relaxation is not needed. 9 of 170 typed benchmark functions (5.3%) are left out for a `match` over any tag, under the stop rule's tenth.
- **Where the build differs from the plan.**
  - The typed write runs before `ryCodeImageWrite` in `code-image.R`, because writing the image ends the recording of the database's closures the file names.
  - **Training versions are written (decision).** With `--train`, every successful `COMPILED` entry of a closure in the package's database is a root before the declared ones, except one whose guess a later conversion of the run contradicted, so the file holds what the workload compiled that declared types cannot reach, such as a version for the type a guessed conversion gave. The file stores a guessed conversion as it stores a checked one, a `Conversion` whose `checked` is empty; at attach, `numbers::assume_conversion` records the version's type as the session's observation, so a session that saw another type refuses the version and a later change makes it stale through `conversion_epoch` (the guessed-conversion bullet under "Typed code calls R natively"). A version for a record or function parameter travels inside its typed caller's code with or without training, so training adds nothing there. Pinned by `install-typed-package.scenario`.
  - Metadata is LEB128 varints in one stream under one hash, not fixed-width struct-of-arrays columns: the loader parses it once into vectors, so the on-disk layout buys nothing, and varints keep files small.
  - Slots are numbered downward from the code (`base - 8 * (n + 1)`), so the table needs no size before the code is laid out.
  - Static strings live in the file's data after the code and `Constant::Str` carries the string; free stubs and data form one "shared" region under one hash, checked at map time, and each version's own bytes are hashed at attach.
  - A version whose direct callee is still attaching misses, so mutually recursive functions compile in the session (self-recursion attaches). An SCC-at-once attach would fix it; not worth it until a real package shows it.
  - Linux maps the file; every other Unix, and Linux where the mapping is refused, copies the code into the same reserved range and flushes the instruction cache (`RY_TYPED_FILE_COPY=1` forces it). Windows does not use the file. The copy path compiles for `aarch64-apple-darwin` but has not run on macOS.
  - Under `ry run --aot`, a package is compiled at load only when its `R/` holds a `.ryt` of another build, because a package with no file cannot be told from one installed with `--ry-no-typed-code` without reading every function (`interp::ahead::namespace_loaded`).
  - The bench engine `ry_typed_file` drives the installed package from a plain R script, because the checker does not resolve names `library()` attaches, so each measured call converts its arguments at the boundary.

## `ry build --aot --exe` appends the script and its code file to a copy of `ry`

**Status:** built. `reference/cli.md` ("Executables") documents the command and `development/typed-code-file.md` ("Executables") the layout.

- **Previous shape.** A script's typed functions compile in every run: at their first call under `ry run`, or before the script starts under `ry run --aot`. Shipping a typed program means shipping its source and ry, and every run checks the script again.
- **Chosen shape.** The executable is `[ry][zero padding to 64 KiB][typed code file][metadata][48-byte trailer]` (`crates/ry/src/executable.rs`). The trailer holds the code file's offset and length, the metadata's offset, length and FNV-1a hash, and an 8-byte magic. The metadata holds the run flags, the script's name, its elaborated text, and the runtime library and base R paths of a development build. `main` reads its own executable's trailer before it parses arguments, so every argument belongs to the script, and a damaged payload is an error, not a silent `ry`. The run skips the checker and goes straight to `ry run --aot`'s path with `interp().ahead.code` set, so `prepare` notes each closure instead of compiling it and the first call attaches through `typed::file::open` with the file mapped from the executable at its offset.
- **Script identity.** A script closure's note is `(SCRIPT_DATABASE = u32::MAX, top-level expression index, position 0)`, stamped with the elaborated text's length and stream hash, which build and run compute from the same bytes. In the code file the script is the package `""`. A closure whose index or text differs never attaches, so it compiles as under `ry run --aot`.
- **Sourced files are read from disk.** Sourced strict files are not embedded: `source()` reads them and they compile as under `ry run --aot`, because embedding needs a virtual file system under `source()` and no real program asks for it yet.
- **A development build's runtime is recorded.** A development build records the absolute paths of its runtime library and base R, because a copy moved out of `target/` cannot find them through its own path; a release records none. The executable hands them to `repl::use_runtime_files` rather than setting `RY_LIBR` and `RY_BASE_R`, so the processes its script starts (which may be another build of ry) do not inherit them, and those variables still win.
- **Reading the payload never stops `ry`.** `main` checks for a payload before the R front ends (an executable may be named `Rscript`), and an executable it cannot read (execute-only, or no `/proc`) is plain `ry`; only a trailer whose payload is damaged is an error.
- **The build protects its inputs.** It refuses an output that is the script itself (an extensionless `main` would otherwise be overwritten) or a directory, makes the copy owner-writable because an installed `ry` is often mode 0555, and removes the partial file whenever writing or renaming fails. `RY_AHEAD_MISS=1` makes the executable compile at first calls, which the scenario uses to check the fallback.
- **Impact.** The executable starts about 1.6 ms sooner than `ry run` on an empty script (no checker), its first calls attach instead of compiling (0.29 ms against 6.6 ms for 20 functions on an iterate build), and its peak resident memory is about 6 MiB lower. Correctness rests on the code file's existing checks; incremental analysis is untouched.

## The typed core lowers R syntax at the first call and runs on 16-byte strings and view lists

**Status:** built.

- **Previous shape.** Strict typed closures ran on the scalar natives in `crates/jit/src/native.rs`, which take R values in and out of every operation, so programs over strings, lists and local functions (`report`, `closures`, `bond_book`) gained nothing.
- **Chosen shape.** `crates/typed` lowers a strict typed closure to a typed SSA IR at its first call for the argument types it sees, inserts Perceus reference counts (`ownership.rs`), and compiles it with Cranelift (`backend.rs`). `jit::native::call` is the one seam: it hands the argument values to `typed::call`, which returns `None` to fall back. A call that fails at run time (an `NA` where an integer cannot hold one, an overflow, an index out of range, an interrupt) returns status 1 and the interpreter reruns it, so the typed core never reports an error itself.
- **Lowering from R syntax at run time, not from salsa.** The runtime cannot link the analysis, and the closure's body is the source of truth the interpreter also runs. The guards on resolved functions (`Speculation`, rechecked on every call) make the lowering sound against rebinding. The serialized per-function IR in `typed-runtime.md` remains the design for install-time code files.
- **`Str` is 16 bytes, not 24.** Two words fit the two-register return of Cranelift's tail convention and halve list storage; strings up to 15 bytes are inline, flagged by bit 63 of the second word.
- **One list header with a data pointer.** A `List` header points at its items, so an R vector argument is passed as a view (flag `VIEW`) over R's own data with no copy, and the same compiled code serves owned and borrowed lists. Writing to a view copies first.
- **Base closures behind lazy-load promises.** Base binds its closures to lazy-load promises, so a function is base's own when its binding cell is base's, and `resolve_function` forces such a promise (it only reads the package database) instead of refusing.
- **Counted loops check their ends once.** `for` over `seq_len`, `seq_along`, `a:b` or a vector tests `counter == end` at the latch and steps with `Op::Step`, which cannot overflow because the counter never passes the end.
- **Impact.** On the why-bench, typed and strict: `closures` 0.348 s to 0.006 s, `report` 0.640 to 0.009, `bond_book` 0.739 to 0.049, `loop_sum` 0.021 to 0.010, `array_sum` 0.025 to 0.004, `array_map` 0.048 to 0.017.

## Typed `I64` runs in registers, and list arithmetic fuses into one loop

**Status:** built.

- **Previous shape.** The typed core had no `I64` register type, so every typed program over `I64` (most of `tools/bench/kernels/*.typed.R` and `tools/bench/applications/*.typed.R`) ran in the interpreter. List arithmetic such as `List.sum(x * 2 + x / 3 - 1)` allocated one full list per operator, and lists over 64 KiB went to the system allocator on every call, which cost page faults on each fresh mapping.
- **`I64` is a native register value.** `I64` scalars live in Cranelift `I64` registers, `+ - *` use checked arithmetic and fail on overflow, and `%%` and `%/%` follow R's floored rules. A failure reruns the call in the interpreter, which reports the error.
- **An `I64` parameter or result converts an exactly whole R number** (`annotations::check_value`, `check_return`, `check_forced`), as an annotated binding does, and the typed core mirrors that conversion at `typed::call`, at inlined calls and at returns. Without it, `f(3)` to an `I64` parameter failed the check, and every benchmark caller had to write `3i64`.
- **Fused elementwise kernels (`Op::Elementwise`, `values::ry_elementwise`).** A tree of base arithmetic over same-length lists and scalars, with at least two list operations or one under `List.sum`, lowers to one small program of up to 16 steps over up to 16 inputs. The kernel runs it in chunks of 128 items, so intermediates stay in L1 and only the final list (or none, under `List.sum`) is allocated. Each intermediate is checked for NaN, as the interpreter checks every operator's result, so a fused tree fails exactly where the unfused one would.
- **`List.sum` over `F64` adds in order in double precision**, in the interpreter and in compiled code alike ("`List.sum` adds doubles in order in double precision" below).
- **Large blocks get power-of-two pools.** Blocks over 64 KiB round up to a power of two, and up to four freed blocks per class, each at most 64 MiB, are kept per thread (`values::block_alloc`). A benchmark that allocates an 8 MB list per iteration went from 407k page faults to 18k.
- **Library constants are speculated by identity.** `Num.pi` and its siblings are read as constants when the value the name resolves to is the very object `standard_library::builtins_env()` holds, so a user binding of the same name, or a rebinding, fails the guard (`lower::resolve_value`, `typed::still_resolves`), and a function that assigns the name anywhere (local functions included) refuses, since a read before a later assignment in a loop would otherwise keep the constant (`lower::assigns`).
- **`Num.pow` is `powf`, not R's `^`.** R's `r_pow` multiplies out small whole powers, which changes the last bit, so the typed core has `Arith::NumPow` for the library function.
- **A counted `List.range` loop keeps the list length limit** (`i32::MAX` items), failing as `List.range` does, though it allocates no list.
- **`List<T>` annotations are checked at run time.** The mask reader knew only the old `Array<T>` spelling, so a closure annotated with nothing but lists counted as untyped and never reached the typed core. `List<T>` requires the Array bit and the item's storage (an R list for records, tags, functions and nested lists), and any empty list passes, because an empty list is stored as `logical`. A `List<I64>` parameter or result converts an R double or integer list of exactly whole numbers item by item (`annotations::as_i64`), because an unsuffixed literal such as `[1, 2]` is a double list at run time. An empty `logical` list becomes an empty `I64` list there too (`annotations::lacks_item_type`), which covers unelaborated code (`ry load`, `source()`) at typed function boundaries.
- **A constant scalar default is passed as the argument** (`jit::native::call`), so a missing argument with a default such as `k: F64 = 3` no longer refuses the call.
- **Impact.** On the why-bench, typed and strict: `array_dot` 3.171 s to 0.004 s, `loop_sum` 2.341 to 0.007, `bond_book` 4.075 to 0.075, `drawdown` 1.663 to 0.007, `closures` 0.756 to 0.004, `vector_arith` 0.266 to 0.062, `log_rank` 3.286 to 1.653 (its `Record` parameter still refuses).

## Array subscripts walk offsets instead of listing them

**Status:** built.

- **Previous shape.** `x[i, j, k]` and `x[i, j, k] <- v` resolved each subscript to positions, then built the column-major offset of every selected element into one list (`flat_offsets`) before gathering or scattering. `X[, , k]` on a 1000 x 1000 x 20 double array wrote an 8 MB offset list per read.
- **Target shape.** `ArrayOffsets` in `crates/interp/src/subset.rs` yields the offsets one at a time, and `select_positions`, `scatter` and `recycle_into` take an iterator. The innermost run walks the first selection longer than one position, so `X[5, , k]` strides through a row with no per-element odometer step; an odometer step updates the base by each changed dimension's contribution and counts `NA` positions, so it is amortized constant time. `next` is inlined and the odometer step is out of line, which keeps the iterator in registers inside the gather loop (without those two attributes, the row read on a cached 100 x 1000 x 20 array ran about 50% slower than with the listed offsets).
- **Impact.** No allocation proportional to the result beyond the result itself. The slab read is 1.7x to 2.5x faster and the row read is unchanged; `NA`, empty and zero-length shapes are pinned by `crates/oracle/cases/assignment__array_slab_shapes.R`. The write `X[, , k] <- W` for numbers already took the slab path and is unchanged.

## Typed boxes have shapes, and a tag type is a union of them

**Status:** built, except the deep conversion of a box result, which "A typed record or tag that R holds stays a box until R reads into it" supersedes.

- **Previous shape.** The typed core knew numbers, `Bool`, `Str`, `I64`, lists and dictionaries. A strict typed closure that built or read a record, tuple or tag, formatted text, or unwrapped a `Result` was refused and ran in tier 0, so `orders`, `expr_tree`, `report` and `team_scores` in `tools/bench/applications` ran interpreted in their typed form.
- **Boxes.** A record, tuple or tag is a counted allocation (`values::Boxed`): a header with the count, the shape id and two bitmasks (the words holding counted pointers and the first words of strings), then one word per field and two per string. Freeing reads only the header, never the shape table. A shape without fields (`:B`, `:Missing`) is one static box per shape (`shapes::constant`), never counted, so the compiler bakes its pointer into code.
- **Shapes** (`crates/typed/src/shapes.rs`). A shape is a heap layout (tag and field names, the same `heap::records::Layout` the interpreter uses) plus each field's item type, so code that knows a box's shape reads a field at a fixed offset without a test. `Type::Record(shape)` names one shape and `Type::Tag(union)` an interned union of shapes. A tag-typed *field* is normalized to any tag (`shapes::ANY`) inside the shape, which keeps a recursive type finite: an expression tree's node shapes do not depend on the shapes below them.
- **Matching any tag.** A tag that crosses the boundary from R has type `Tag(ANY)`, because the runtime does not check tag-union annotations. A `match` over it switches on the shape id (`Op::ShapeOf`) over the *confirmed* shapes whose layout or tag a pattern names, and gives up for any other shape. A shape is confirmed when the boundary converts it, or when code that constructs it compiles: the lowering lists the shapes it constructs (`Lowered::constructed`) and `typed::compile` confirms them only once the whole function lowers, so a refused lowering confirms nothing. Confirming can grow the set a `match` in the same function relied on (a loop that wraps its own value), so `compile` lowers again until the epoch it read is the current one. The compiled code records that epoch, and `typed::compiled_for` compiles again once the confirmed set grows. When an arm does not lower for one confirmed shape (a payload of another type than the annotation says, which only ill-typed R code passes), `Lowering::rewind` drops what the arm emitted and that shape alone gives up, so one stray value from R does not refuse the whole function. A stray arm can also lower and give a value of another type (`:One(x) => x` once `:One(2.5)` confirmed `One(F64)` beside `One(I64)`), and then the arms do not join. So a `match` whose value is the function's result (`lower::tail_matches`: the body, a block's last expression, `if` branches, arm bodies) lowers each arm against the declared result type (`typed::declared_result`; `Lowering::switch_shapes`'s `target`): a number converts to a declared `I64` inside the arm as the exit would, and an arm for an only-confirmed shape whose value still cannot have the type is rewound. A `match` elsewhere has no declared type to judge by and keeps refusing when its arms disagree.
- **Ry lists in a tag from R** (`typed::list_of_array`). The boundary converts a payload that is an `Array` into an owned native list, deeply, taking the item type from the value: an atomic `Array` stores it, and an `Array` of records, tags or lists is an R list whose items must share one type. The declared type is not threaded into the conversion because it need not be: compiled code that meets a shape whose field type it cannot use gives up, and the interpreter makes the call. The one value that cannot say its type is an empty logical `Array`, which is what a bare `[]` is whatever list it stands for (a `[]` the checker typed is `.ry_items([], "I64")`, which has `I64` storage), so it refuses rather than confirm a `List<Bool>` shape no code expects. Every refusal at the boundary carries its reason into the `RY_TYPED_LOG` line.
- **Boundary.** A record or tag argument from R converts deeply into boxes (`typed::to_box`), and a box result converts back. Plain record and tuple *parameters* are not taken from R (`type_of` gives `None`), because the typed-record-parameter design is parked; a record passes natively only between typed closures (`CallTyped`). The deep conversion dominates `expr_tree` (`backlog.md`).
- **Typed definitions with alias-only annotations.** `annotations::register_definition` used to skip a closure whose every annotation had an empty runtime check mask, which is exactly a closure annotated only with aliases or tag unions (`price_line(order: Order)`), so it was never marked typed. It now skips only a closure with no annotation text at all.
- **Typed calls are never read ahead.** `native::pure_value` does not evaluate a typed closure's call inside an unforced argument promise, although its native code changes nothing visible: the call may not finish, and R never runs an argument its callee does not force (`ignore(1, spin())` must return). So `evaluate(build(14, 8))` runs `evaluate` in the interpreter, and `expr_tree.typed.R` binds the tree first.
- **Impact.** All four applications run natively with checksums equal to tier 0's. Correctness rests on the shape switch being exhaustive over what compiled code can see, which the epoch guarantees. `test_tiers::typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` pins tier agreement, including the ill-typed payload case.

## Typed-to-typed calls are eager on every tier

**Status:** built.

- **Previous shape.** `Op::CallTyped` in compiled typed code computed every argument before the call, while tier 0 left each argument a promise until the callee forced it, so the tiers disagreed on an argument that never finishes or has an effect. A typed closure the typed core refuses (`log_rank`, over a record parameter) ran its body in the interpreter, where a call of a local typed function with an argument `pure_value` cannot read (`count(risk & data$treated)`) also missed the native path: the callee ran interpreted, `List.count_if` called an R closure per item, and the typed program took 6.4 s against 0.21 s as plain R.
- **Target shape.** Typed ry is strict: a call from one typed function to another computes every argument the caller wrote, left to right, before the callee runs. `interp::eval::force_typed_arguments` forces, before a typed callee is entered, each argument promise whose environment is the frame of a live typed closure; a typed closure never runs on slots, so the environment test is exact. A promise written in R code, including one forwarded through `...`, stays lazy, so calls from R keep R's semantics and `native::pure_value` still never reads a typed call ahead.
- **Defaults.** `native::gather` also passes a default that names no formal and that `pure_value` reads in the closure's environment (`dt = 1f64 / 12f64`), since the frame could give it no other value.
- **Impact.** Native and interpreted typed code agree by construction, `log_rank` typed runs at plain ry's speed (0.25 s), and the one documented difference from GNU R running the erasure is this argument order (`type-system.mdx`, "Annotations at run time"). `test_tiers::typed_to_typed_calls_evaluate_their_arguments_first` pins it with the JIT off and on.

## R lists do not convert to records at a typed boundary

**Status:** built, except the "Gap" bullet, which "A ry list is one item word" supersedes.

- **Rejected: record parameters filled from R.** The rejected design let a typed function take an R list, or a record R built with `as.record()`, as a `Record{...}` argument: the boundary read each field's type, and the compiled function received the fields spread out as separate parameters. The owner rejected it.
- **Why.** An R list is untyped and mutable, so accepting one as a record makes the boundary implicit: the field types, lengths and `NA`s are checked deep inside a call instead of where the caller converted them, and a second conversion route sits beside `List.from_r()` and `Num.from_r()`. Spreading the fields also gave the typed core a second kind of binding (a record known only field by field) with its own refusals, such as a record used whole or assigned.
- **Target shape.** R values enter typed code only through the per-parameter boundary, as plain vectors and scalars checked at entry. Records, tuples and tags are built inside typed code. A plain R list where a record is expected is a `type-mismatch` (`reference/type-system.mdx`, "Records from R"); a record from `as.record()` passes the check but runs in the interpreter, which `RY_TYPED_LOG` names. The typed log-rank benchmark (`tools/bench/kernels/log_rank.typed.R`) takes its columns as `List` parameters.
- **Gap.** A record built inside typed code cannot yet hold a `List` field (`shapes::intern` stores only scalars, strings and boxes), so a typed function cannot bundle its vector parameters into a record natively (`backlog.md`).

## A typed call that runs in the interpreter always says why

**Status:** built.

- **Previous shape.** The native path (`jit::native::call` into `typed::call`) answered `None` from about a dozen places. `annotations::log_interpreted` printed only ``typed: `name` runs in the interpreter``, and a compile refusal printed on its own line without the function's name, so a fallback for an argument the native tier could not read, a debugged closure, a set result or code that gave up at run time left no reason at all. The bench ports could not tell a slow typed number from a silent fallback.
- **Target shape.** Every point where the native path declines calls `annotations::decline(|| reason)`, and the interpreter's fallback prints ``typed: `name` runs in the interpreter: <reason>`` once per name and reason. The reason closure runs only under `RY_TYPED_LOG`, so a decline costs one test of a cached flag. The compile table keeps each refusal's text, so a cached refusal still gives its reason. A method reached through dispatch and a session with no native tier log from `eval::try_native_typed`. `RY_TYPED_LOG` is the typed core's explain path; ``the native path recorded no reason, which is a bug`` marks a decline point that was missed.
- **Unforced promises are reported, not forced.** An argument R code wrote stays a promise until the typed function uses it (`reference/type-system.mdx`, "Parameters are checked when their value is computed"). Forcing it at the boundary would run its code before the function body, which changes the order of effects against the body and runs an argument the function never uses, so it is not done even where the parameter type needs a value. The native tier reads ahead what cannot run R code (constants, variables, forced promises and base arithmetic over them, `native::pure_value`), and anything else is a decline whose reason quotes the argument and says to compute it into a variable first. Calls from typed code to typed code force their arguments first, because typed ry is strict.
- **Pinned by** `typed-fallbacks-give-a-reason.scenario` and `test_scenarios::typed_benchmarks_run_natively`, whose failure message is the fallback lines with their reasons.

## Typed code calls R natively

**Status:** built.

- **Previous shape.** The typed core refused any call of an R function, so one `set.seed` or `stats::rnorm` sent the whole typed closure to the interpreter (`vasicek` ran untyped speed). The only way across was to move the R call out of the typed function.
- **Target shape.** A call of an untyped R function (a base function, an R closure, or `pkg::name` of a loaded namespace) compiles to `Op::CallR`, which calls `typed::ry_call_r` with a leaked, globally rooted `lower::RCall` site. It compiles in exactly two positions: as a statement whose value is unused, and as the direct argument of `Num.from_r`, `Str.from_r`, `Bool.from_r` or `List.from_r`. Anywhere else the value would be an untyped R value, which typed code cannot hold, so the refusal says to convert it.
- **Same semantics as the interpreter.** Each argument reaches the function as a forced promise of the code written in the call, so `substitute()`, `sys.call()` and error calls match. `Num.to_r(x)` and `List.to_r(x)` arguments, and the result conversion, run the real standard library functions through `eval::apply`, so their errors are the interpreter's. An `I64` (or a list of them) crosses only through `Num.to_r` or `List.to_r`, and a set or dictionary never does. An R call inside another R call's arguments is refused, because R's lazy arguments decide that order and compiled code computes arguments first.
- **Result types come from feedback.** `Str.from_r` and `Bool.from_r` give a fixed type, but `Num.from_r` and `List.from_r` give `F64` or `I64` (or a list of `F64`, `Bool`, `Str`, `I64`) depending on the R value. Each run of those conversions in the interpreter records the R type by call (`numbers::observed_conversion`). Without a record, lowering refuses with a refusal starting `lower::AWAITING`; `typed::AWAITING` remembers what it waits on (`lower::Awaited`), and the closure compiles again once the interpreter has run the conversion, so the first call runs interpreted and the rest natively. A `pkg::name` call whose namespace is not loaded waits the same way, because resolving it at compile time would load the namespace, which only running the code may do. A conversion the checker typed (`Conversion::checked`) gives its type or an error; one it did not is a guess (`Conversion::guessed`), handled below.
- **A failed guess runs the call again in the interpreter (decision).** Previous shape: the code compiled for the type an unchecked `Num.from_r` or `List.from_r` gave first, and a later value of another type was an R error ("the typed function was compiled for the type it gave before"), which the interpreter, with no compiled code, never raised. Target shape: `numbers::Observed` tracks `Steady(type)` or `Changed(first, second)` per conversion call, and a change bumps `numbers::conversion_epoch`. In native code a mismatch of a guessed conversion sets `typed::FAILED_GUESS` and gives up. Code that guessed holds the epoch at compile (`Compiled::guessed`, the earliest of its own and its callees') and is stale once it moves, as with the shape epoch; lowering refuses a `Changed` conversion ("gave `F64` in one call and `I64` in another"), so the function runs in the interpreter from then on. Impact: every tier prints what the interpreter prints, at the cost of one more compile per change.
- **Giving up after an R call replays it (decision).** Previous shape: compiled code that gave up after an R call could not hand the call to the interpreter, since running it again from the start would repeat what the R call did (print, draw a random number, assign), so giving up there was an error, and so was any failed guess. Target shape (`interp::replay`): each `typed::call` opens a segment, and `ry_call_r` records `(call node, value)` for every R call that is not `effect_free` or that warned (`NULL` for an unused value); `replay::trace` marks the recorded values, so a native loop roots nothing per call. On a give-up after such calls, `replay::replay` runs the call in the interpreter (`eval::run_interpreted`) with the segment's records pending. While records are pending, `run_frame_general` evaluates a typed closure's body through `replay::body`, a copy in which each pending call node is wrapped as `.ry_replay(rewritten, original)`; that special returns the first unused record for the node (innermost replay first), marking the records of calls nested in it used (their values were folded into it, and a later loop iteration must not take them), or evaluates live. Native `ry_call_r` consumes records the same way and records the value again, so a typed callee that gives up during a replay replays too. Typed code computes only from its arguments and its R calls, so the interpreter reaches the recorded calls in the order the code made them. Rejected: rerunning the call whole (repeats effects), hooking every interpreter call path (too many), and running a copied closure (loses the signature, which is keyed by formals and body). A segment records at most `replay::MAX_RECORDED` (4096) calls and `replay::MAX_RECORDED_MIB` (64) MiB of values (each value's own bytes and its list elements'), since the records keep every value alive; giving up after more is an error that says so. Code attached from a typed code file holds the body's own call nodes (`file::Place`: the holder closure and the item positions to the call), because replay and `numbers::assume_conversion` key on the node; a rebuilt copy made attached code repeat its R calls on a give-up and never refuse a guessed conversion. An error an R function raised (parked in `PARKED`) is still raised as is, and `stop(...)` is an R call site marked `diverges` followed by a give-up, so R raises the user's message and the error parks. A diverging site does not set `calls_r`, since it cannot change a variable a lazy argument reads. Impact: correctness, every give-up is now sound; performance, nothing on a call that does not give up beyond one thread-local push per effectful R call; while records are pending, every typed closure the interpreter calls builds a rewritten copy of its body (a hash set of the pending nodes and a walk), which only a give-up pays.
- **R calls run in a frame for the typed call.** Native code has no frame, so `ry_call_r` pushes one (`eval::run_in_frame`) from the `TypedCall` the interpreter hands the tier (`typed::RUNNING` holds it while the code runs), with a fresh environment enclosed by the closure's. `stop()`, `sys.call()` and `parent.frame()` in the called function then see the typed call, as in the interpreter. The frame costs an environment per R call, and only R calls pay it. A typed callee that compiled code calls directly reports its caller's call.
- **Lazy arguments.** The native tier reads some promise arguments ahead without forcing them (`native::pure_value`). If such a value depends on a variable, an R call in the body could change that variable before R would have forced the argument, so code that calls R (`Compiled::calls_r`, which includes the functions it calls) declines those calls with a reason.
- **Pinned by** `typed-code-calls-r.scenario` and `test_scenarios::typed_benchmarks_run_natively`, which accepts the one feedback fallback line per conversion.

## Typed list literals compile natively, and an empty list of boxes settles its item type at first use

**Status:** built.

- **Previous shape.** The typed core had no case for `.ry_array`, so `[a, b]` and `[]` read as an R call whose value typed code cannot use, and the whole closure ran in the interpreter (bounce, binarytrees, nbody, fasta, pidigits, scalar-while). `List.append` was not compiled either.
- **Target shape.** `[a, b, ...]` lowers to an empty vector of the items' common type and one `Op::Push` per item (`list_literal` in `crates/typed/src/lower.rs`); items of different types refuse. `List.append(list, item)` is `Op::Push`, which grows a uniquely held list in place with doubling capacity. `.ry_items([], "I64" | "F64" | "Str")` gives the named item type, and a bare `[]` is a `Bool` list, because the preflight wraps every other empty list.
- **Empty lists of records or tags.** The preflight names only their storage (`"list"`), and typed values take their types from the flow of values, not from the checker. So such a `[]` starts as a list of any tag and is *pending*: the first `List.append` or assignment that meets a concrete list settles it, retyping every local that holds it and the allocation in place (`Lowering::settle`). Settling refuses when anything but a copy or a length read the placeholder, since such an operation chose its code by the placeholder type, and reading the items of a pending list refuses up front. One never settled stays an empty list of any tag, which is a sound type for a list that holds nothing.
- **Why not carry the checker's type.** Passing a full type through the preflight would need a type printer and a parser for records, tuples and tag unions in the runtime, a second description of the same type. Every case in the ports appends before reading, so flow typing settles them all.
- **Pinned by** `typed-list-literals.scenario`.

## A count of a logical list fuses with the comparisons that built it, after lowering

**Status:** built.

- **Previous shape.** `List.count_if(time >= t & event, \(b) b)` built a `List<Bool>` per comparison and per `&` only to count the last one. Typed `log_rank` builds four 16,000-item lists for each of its 1,000 event times, which kept it at untyped ry's time.
- **Target shape.** `count_if` with the identity predicate over a `List<Bool>` lowers to `Op::CountTrue` over that one list. A pass over the IR (`crates/typed/src/fusion.rs`), run before ownership insertion, follows the list back through `VectorCompare` on `I64` or `F64`, `VectorLogic`, `VectorNot` and same-typed `Copy`, and turns the count into one program over the comparisons' list and scalar inputs. `values::ry_count_true` runs it in chunks of 128 items with lanes 0, 1, 2 for `FALSE`, `NA`, `TRUE`, so `&` is `min`, `|` is `max` and `!` is `2 - x`. It fails, and the interpreter reruns the call, on `NA` in the result, a length mismatch or an invalid program, as the unfused code would. Absorbed instructions whose results nothing else reads are removed. Several counts over the same lists then share one pass ("Counts that share a list share one kernel pass" below).
- **Why after lowering, not during it.** At lowering time an operand is a local that may be reassigned, read in a branch, or also returned, so fusing there means deferring bindings across all of that. The IR pass sees the finished code and only fuses within a straight-line run that ends at the count (the count's block and the chain of single-predecessor blocks that jump into it), where every instruction runs once before the count. It checks that no input is reassigned between the comparison that read it and the count.
- **Comparisons and `!` on ry lists give ry lists.** A comparison with a ry list on either side, of any item type, and `!` on a `List<Bool>` give a `List<Bool>`, not an R logical vector, so `&` and `|` with another ry list accept them (`compare` and the `!` case in `lower.rs`).
- **Impact.** Typed `log_rank` drops from 0.213 s to 0.112 s median on a plain release build (untyped ry 0.227 s), with the same checksum.
- **Pinned by** `typed-mask-counts.scenario` and `fused_counts_follow_three_valued_logic` in `crates/typed/tests/test_values.rs`, a seeded model of R's three-valued logic.

## A uniquely owned Array grows in place

**Status:** built.

- **Previous shape.** `owned_list` in `crates/interp/src/standard_library.rs` excluded Arrays (ry lists built by `[]`, `List.from_r` or typed code), so every `List.append`, `List.set`, `List.concat` or `List.swap` on one copied and re-checked all its items. A loop of appends was quadratic: 40,000 took about 25 s, although the standard-library page promises amortized constant time.
- **Target shape.** The same ownership rule as for R lists (`owned`: refcount 0, or the reusable binding with refcount 1) applies to a non-empty Array. An update first checks the new item against the Array's storage without allocating (`arrays::holds_item`), then stores it in place (`arrays::store_item`) or appends into spare capacity, doubling into a resizable copy when it runs out (`append_owned`). A value that does not fit takes the copying path, which reports the error and never sees a half-updated Array. An empty Array is `logical(0)` until its first item decides its storage, so it always takes the copying path once. The moves journal (`crates/interp/src/moves.rs`) covers Arrays too, recording replaced items so a failing helper restores the original.
- **Why not a separate List representation.** An Array is already the one storage of a ry list in both the interpreter and typed code (typed `List.append` is `Op::Push`, which grows a uniquely held list in place), so the fix is ownership, not a new type.
- **Impact.** Two million interpreted appends to `[]` take about 0.2 s. rcp `list-for` and `list-while` get typed ports (`tools/bench/public/rcp/*.typed.R`) that run in about 0.015 s, against about 0.35 s untyped.
- **Reserved room.** `List.with_capacity` and `List.reserve` of an empty Array give an empty resizable `logical` Array whose capacity is the room. The first append builds the Array in the storage its items decide with that capacity (`append_owned`), and `.ry_items` keeps it when it retypes the empty list. Typed code lowers `.ry_items(List.with_capacity(n), "I64")`, which the preflight writes because the checker alone knows the item type, to `Op::EmptyList`.
- **Pinned by** the Array growth and `move_undo` cases in `crates/interp/tests/test_standard_library.rs`.

## A ry list is one item word

**Status:** built.

- **Previous shape.** `ir::Item`, the type of a list item or a record field, had scalars, strings and boxes only, so a record with a `List` field or a `List<List<I64>>` refused, and pidigits (a big integer is a record of limbs) ran in the interpreter.
- **Target shape.** `Item::Array(id)` is a ry list held as one counted pointer word. `id` interns the list's item type in `crates/typed/src/shapes.rs` (`array_of`, `array_item`), which keeps `Item` `Copy` and one word however deep the nesting. `Type::held_item` gives the item a value is stored as, and the places that store a value (list literals, `List.append`, record construction) use it; `Type::item` is unchanged, because comparison, `rep` and vector conversion must keep refusing a list of lists. `ry_list_drop` already dispatches on the header's kind, so a box releases a list field like any object word. In R, a list of ry lists is an R list of Arrays.
- **Bool and logical join.** A `Bool` is a logical that is never `NA` in the same word, so a branch or `return` of a `Bool` against a logical joins as a logical (`joined` and `exit` in `lower.rs`). pidigits' comparison helpers return a comparison in one branch and `Bool.false` in another.
- **An empty `I64` sum is 0.** `ry_sum_i64` and the summing programs of `ry_elementwise` give 0 for an empty list, as the standard library does for an empty list of either kind (`I64` storage survives an empty subset in the interpreter), instead of handing the call back.
- **A copying push grows from the length.** `ry_list_push` on a shared list copies it with room from its length, not from the shared list's capacity, which otherwise doubled on every copy in a loop of field updates and asked for gigabytes after about 30 steps.
- **Impact.** Typed pidigits runs natively, with the same checksum, in about 0.03 s against about 0.45 s untyped.
- **Pinned by** `typed-record-list-fields.scenario` and the list-field cases of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` in `crates/jit/tests/test_tiers.rs`, which also check that nothing leaks.

## Typed `List.sort_with` is a merge sort with its comparator inlined, and strings order by the interpreter's collation

**Status:** built.

- **Previous shape.** `List.sort_with`, a tuple index such as `entry[[2]]` and `<` on `Str` refused, so typed knucleotide ran in the interpreter.
- **Sorting.** `sort_with` in `crates/typed/src/lower.rs` builds a bottom-up merge sort in the IR: each pass merges runs of `width` items from one buffer into a second, and the buffers trade places after the pass. The comparator is inlined once into the merge step, as `List.map` inlines its function, so no closure call or callback into native code is needed. A right item goes first only when `compare(left, right)` is `:GT` (tested with `Op::ShapeOf` against the `:GT` shape), which keeps the sort stable, as the standard-library page promises. A native sort routine that calls back would need a compiled comparator with a native entry point, which inlined lambdas do not have.
- **Tuple index.** `[[` on a tuple with a constant position is `Op::Field`. A computed position, or any index of a named record, refuses, because a field's type depends on the position.
- **String order.** `values::str_order` calls `interp::sort::collate`, the same function as R's `<` in the interpreter, for scalar comparisons (`ry_str_compare`) and list comparisons alike. The interpreter reruns a call that fails natively, so the two tiers must agree in every locale; calling one function is the only way to keep that true, and `collate` reads the locale on every call, so `Sys.setlocale` is honored too.
- **Impact.** Typed knucleotide runs natively in about 0.04 s, against about 0.5 s untyped, with the same checksum.
- **Pinned by** `typed-sort-with.scenario`, the sort and string cases of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` in `crates/jit/tests/test_tiers.rs`, and `comparisons_and_logic_follow_r` in `crates/typed/tests/test_values.rs`.

## Converting a record tree at the typed boundary takes one shape-table borrow and no per-node allocation

**Status:** built.

- **Previous shape:** `to_box` interned each node's shape through a hash map keyed by a freshly built `(layout, SmallVec<Item>)`, took separate thread-local borrows to intern, confirm and fetch the static box, cloned the whole `Shape` (two `SmallVec`s) to learn its offsets, and gathered the fields into `SmallVec`s before copying them into the box. `from_box` cloned the `Shape` per node as well. On `expr_tree` typed, where every iteration converts a tree of about 570,000 nodes held by R, the conversion was most of the run.
- **Target shape:** shapes are found through `by_layout`, a dense vector indexed by layout id (layout ids come from an append-only interner), with a short scan of that layout's shapes and no hashing or allocation on a hit. `shapes::with` lends the whole table to one conversion, and `Shapes::converted` interns, confirms and returns the words, masks and static box in one call. Fields convert into stack buffers already laid out as the box lays them out (offsets are prefix sums of `item_words`), so the box is filled with one copy. `from_box` copies out only the layout and the items under a short borrow, because allocating an R record may collect and so may run R code that reaches the table.
- **Stack growth:** the conversion checks the remaining stack every `STACK_CHECK_LEVELS` levels rather than once at the top, so a 200,000-level chain converts both ways, and the thread-local stack query stays off most nodes.
- **Impact:** warm Ir per `expr_tree` typed iteration fell from about 429M to about 226M. What remains is per-node work the R representation forces (freeing the boxes, allocating the records); removing it needs typed values to stay native while R holds them.

## A typed record or tag that R holds stays a box until R reads into it

**Status:** built.

- **Previous shape:** every record or tag a typed function returned to R was converted the whole way down into R records (`typed::from_box`), and passing it back to typed code converted it the whole way back into boxes (`typed::to_box`). On `expr_tree` typed, which builds a tree natively and hands it to R between calls, that round trip was about 227M of 230M warm instructions per iteration.
- **Target shape:** `from_box` returns a lazy record: an R record of the right layout whose layout word carries `heap::object::LAZY_RECORD`, whose slot 0 holds the box, and whose slots are all marked raw, so the collector traces nothing in it. Its refcount is `REFCOUNT_MAX`, so no in-place update ever writes to it. `interp::records::slot_value` and `with_slots` fill it on first read (one level, the children become lazy records in turn), and every other slot reader goes through them; `identical()` fills through `settle_uncollected`, which holds off collection, and compares two lazy records over the same box without filling either. `to_box` of a lazy record takes another reference to its box and converts nothing. `Interp::lazy_records` owns one reference per lazy record and releases it at the collection that finds the record dead or filled, which is safe because the heap never moves objects.
- **Compiled code:** a lazy record fails the JIT's exact layout compare for `$` and the native field load (`jit::native::field` checks the sign of the layout word), so it takes the interpreter's path, which fills it; the layout branch masks the bit off; `argument_word` does not pass one as a native argument.
- **Native memory:** boxes are malloc'd, so a program that keeps creating lazy records grows native memory the collector cannot see. `from_box` runs a minor collection once the typed core's live object count passes a threshold (at least 2^20), and then doubles the threshold past what survived.
- **Impact:** warm Ir per `expr_tree` typed iteration fell from about 227M to about 24M, with the same checksum. What remains: a record R built itself still converts deeply on every call, and a filled record keeps no link to its box.
- **Filling roots the record first**, because the reader may hold it only in a register and a collection mid-fill would otherwise release the box the fill reads.
- **Pinned by** `typed-lazy-records.scenario`, which also runs under collection stress with the JIT forced on.

## The typed suite scores typed ry against Rust

**Status:** built.

- **Previous shape.** The only measure of typed ry against Rust was the speed score of `just climb` (`tools/climb/run.py`), which timed Rust twins kept in `tools/climb/rust/` with a statistic of its own: each twin printed the best of three warm runs, while `tools/bench/harness.typed.R` printed the median of five. The score was unpublished, three twins had drifted from the sizes of their typed programs, and nothing compared their checksums in the published runner.
- **Target shape.** `tools/bench/typed/` is a suite like the others: a `suite.toml` that names each program's `typed_source` (the file the kernels or applications suite owns) and `rust_source`, the twins, and `harness.rs`, which every twin includes and which prints the typed harness's line (`name checksum median min cold`, repeat count from `RY_BENCH_REPEAT`). `bench.py` runs it with Rust as the baseline and typed ry as the measured engine, needs `rustc` and no GNU R, and fails a program whose checksums differ. The climb loads `bench.py` and times through the same runner, so one copy of the twins and one statistic exist.
- **A results file names its baseline and the direction of its ratios.** `baseline` is `gnu_r` or `rust`, and `ratio_kind` is `speedup` (baseline time over engine time) or `slowdown` (engine time over baseline time). A typed-over-Rust ratio is a time, where lower is faster, so the renderers keep it out of every table, chart and sentence of speed-ups over GNU R and give it a paragraph and a section of its own. A file without the two keys reads as `gnu_r` and `speedup`.
- **Why `rustc -O` on one file.** `-O` is optimization level 3, the level of cargo's release profile, with no target-CPU flag, so a twin is built as a released Rust program is, and a single file needs no cargo project or lockfile per program. The flags and the compiler version are stored in `meta.rust`.
- **The too-short floor is per suite.** Both harnesses time warm runs inside the process, so start-up is outside the time and a run of a few milliseconds is stable. The typed suite's floor is 0.1 ms on Rust (`RUST_SHORT`), against 0.1 s on GNU R elsewhere, and is stored as `meta.too_short_seconds`. The program sizes stay those of the kernels and applications suites, which share the typed files.
- **The typed harness reads `Sys.time()`.** `proc.time()`, which `harness.R` reads, is a millisecond clock, which is too coarse beside a Rust program that finishes in under a millisecond, and the harness prints six decimals for the same reason.
- **No peak memory.** `wait4` reports a child's peak resident set no lower than the runner's own at the fork, and a Rust twin needs less than a Python process, so the suite stores no `peak_mib` and its section has no memory table.
- **`--any-build`.** `bench.py` refuses a binary that is not a release build, because its numbers are not publishable. The flag lifts the refusal for a local look (`just bench typed --ry target/iterate/ry --any-build`); `--write` still requires `--release`.
- **Impact.** "Typed ry is N times Rust" is a number `bench.py` computes and the renderers display, with the same checksum gate, paired rounds and intervals as every other suite.
- **Pinned by** `TypedSuite` and `BuildProfile` in `tools/bench/test_bench.py`.
- The bar for the suite's geometric mean is 2x Rust's time or better (user direction). `just bench typed --ry PATH --any-build` scores a local build against it without the release archive.

## Nativeness is a strict-mode diagnostic

**Status:** built.

- **Previous shape.** Whether a typed function ran on native code was known only to the typed core. A function `lower.rs` refused ran in the interpreter with the same output, and the only signal was a line on stderr under `RY_TYPED_LOG`. The checker held no notion of "native", so a refusal showed up as benchmark time, and the only guard was `test_scenarios::typed_benchmarks_run_natively` for the benchmark programs.
- **Target shape.** `semantics::native::native_blockers` judges each native function after its item is checked, from the item's HIR and recorded types, and stores the blockers in `ItemCheck.native_blockers`. `strict_diagnostics` renders them under two codes (next bullet), so they show in `ry check` and in the editor, and the errors reach the gate that refuses to run strict code (`preflight`). It is a pass over one item, not a query, so the per-item firewall holds and it reads other definitions only through the declared signatures the check already resolved. The contract is `reference/type-system.mdx`, "Native functions".
- **Which functions are judged.** A function literal whose declared signature (inline or `#:`) is written entirely in ry types: every parameter annotated with one, the result one, `NULL` or absent, and at least one ry type. A signature with an R type is the opt-out: such a boundary function is where R values and ry values meet, it may run in the interpreter, and it is not judged. The signature decides, because it is what the author wrote and what every caller sees, and because a function that takes or returns an R value cannot avoid holding one. An unannotated function literal is judged only inside a native function. A signature in a `#:` comment or on the assigned name is judged by the same rule and compiles like the inline form ("A function has one signature, resolved in `syntax::annotation`"); only a signature that is the name of a type alias gets an `interpreted` warning on top (`Uncompiled::AliasedSignature`). A type parameter bounded by an R type (`<T: numeric>`, `native::has_r_bound`) makes a boundary function, so `ry check --annotate` output on plain R code is not judged as ry.
- **Two codes, two severities.** An R value in a native function (`NativeBlockerKind` other than `Uncompiled`: an R call's value, a nested R call, an outside variable, a column read, `a:b`, an R literal, a stray `*.to_r`) is a `native` error, because it breaks the rule the function is written under and the author can fix it. Legitimate ry that the typed core does not compile yet (`Uncompiled`: `try`, list patterns, a standard library function without native code) is an `interpreted` warning that says the function runs in the interpreter and how to write the step so it compiles. It is a warning because the gap is the compiler's: as an error, strict mode refused about 195 standard library functions and `try`, and the docs examples for them had to leave strict mode. A warning keeps the property visible at the construct without rejecting the language.
- **The rule in the body.** Every value is a ry value. An R function may be called as the direct argument of a `*.from_r`, as a statement whose value is unused, and as `stop(...)`. Table values (`DataFrame`, `LazyFrame`, `GroupBy`, `LazyGroupBy`, `Expr`) are ry values, and a table function that gives one is a native call (`native::gives_table_value`, which must agree with `gives_table_value` in `crates/typed/src/lower.rs`).
- **Sound in one direction.** A native function with neither finding compiles in the typed core. The checker over-reports where it cannot decide: base R functions the typed core computes itself (`sqrt`, `nchar`, `paste0`) are still R functions to the rule, and `a:b` is reported although `for (i in a:b)` compiles. `native::COMPILED_STD` mirrors the standard library functions `Lowerer::builtin` has code for, because `semantics` cannot depend on `typed`; the agreement test fails for a listed name that no natively running function of its corpus calls, so a name the typed core lacks cannot stay listed. An operator that builds an `Expr` is told from arithmetic by the form of its operands on both sides (`Lowerer::is_table`, `Judge::table_operand`): a variable, an operator, or a call of a table function or of a top-level function that declares a table result.
- **Each function is judged alone.** A caller of a blocked native function gets no finding, and an R value is reported once, where it first appears, as `strict_origins` does for an undetermined value.
- **Not covered.** The rule judges a function's body, not its calls. A call from R code that the native entry declines (a record, tuple, list literal or function argument, an argument that is not a constant or variable, a variable argument to a function that calls R, a `Dict` or `Set` result) runs that call in the interpreter without a finding, and so does the first call that waits on the interpreter to load an R function or to learn a conversion's type.
- **A variable keeps one layout.** The typed core gives a variable one machine type, so the judge tracks each local's type and reports an assignment whose value has another layout (`Judge::layout_differs`, which mirrors the lowerer's value classes: scalar kinds, list, record and tuple shape, tag set, table). It applies to parameters, loop variables and match bindings as well, and an `if` keeps a name only when both branches agree, so the value of an `if` without `else` is a finding. A local function is walked where it is first called, not where it is defined, because it reads the caller's variables as they are at the call.
- **Pinned by** the `native` group in `crates/semantics/tests/typing-strict/native.R.test` and by `test_scenarios::native_diagnostics_agree_with_the_typed_core`, which checks a corpus in strict mode, runs it with `RY_TYPED_LOG`, and fails when a function without a finding falls back or a `COMPILED_STD` name goes unexercised. `NATIVE_CHECK_EXCEPTIONS` there lists the functions the checker passes and the typed core still refuses; an entry that starts compiling fails the test, so the list only shrinks. Programs under `crates/ry/tests/native/` state their blocked and boundary functions in a header.
- **Impact.** Correctness: a typed program that is clean in strict mode runs natively, apart from the pinned exceptions and declined calls. Simplicity: one rule about values replaces reading a log. Performance: none in the checker beyond one walk of each judged function. Incremental analysis: unchanged, because the blockers live in the per-item check.

## Typed code holds table values natively

**Status:** built.

- **Previous shape.** A typed function with a `DataFrame` parameter never compiled: `typed::type_of` had no type for a table, so the whole function ran in the interpreter. The typed `sales` benchmark was the one typed program exempted from `test_scenarios::typed_benchmarks_run_natively`.
- **Target shape.** `ir::Type::Table` is one opaque word: the pointer of a DataFrame, LazyFrame, GroupBy, LazyGroupBy or Expr (`frames::dataframe::is_table_value`). Typed code cannot look inside it. It passes it to the table API, whose functions (`gives_table_value` in `crates/typed/src/lower.rs`: every `DataFrame.*`, `LazyFrame.*`, `GroupBy.*`, `LazyGroupBy.*` and `Expr.*` function that gives a table value, plus `DataFrame()` and `as.DataFrame()`) compile to `Op::CallR` sites with `RResult::Table`. A table value is not counted: the collector owns it.
- **A table call is not an R call for the two R-call rules.** The table API changes nothing a second run would see, so a table call may nest in another call's arguments (compiled code computes arguments first, and no order is observable), it does not set `Compiled::calls_r`, and it does not count in `R_CALLS`, which keeps a give-up after it a plain re-run in the interpreter. The one visible effect is a warning, so `ry_call_r` compares `interp::conditions::warnings_raised()` around a table call and counts the call when the number moved.
- **Operators.** An arithmetic, comparison or logical operator with a table operand (`Expr.col("units") > 3`) is a table call of the base operator, which dispatches to the `Expr` method. The lowerer decides it from the syntax (`Lowerer::is_table`): a variable holding a table, a table function call, a parenthesis or operator over one, and a call of a typed function whose written result type starts with a table type's name (`lower::returns_table`). The runtime keeps no alias definitions, so a result type that is an alias of a table type is not seen there; the checker, which does resolve the alias, warns and asks for a variable.
- **Arguments cross as for any R call.** An `I64` reaches a table function through `Num.to_r`, as the checker requires (`Expr.col("units") >= least` with an `I64` is a `type-mismatch`), so the lowerer has no table-only argument rule.
- **Columns are R vectors.** `table$name` and `table[["name"]]` compile to a call of base `[[` and are legal only where an R call's value is (inside `List.from_r` and the other conversions, or as an unused statement), because a column is an R vector that typed code cannot hold. `table[...]` is refused with a message that names the verbs.
- **Rooting.** A table result lives in a machine word the collector does not scan, so `ry_call_r` roots it on the handle stack, and the outermost `typed::call` releases its mark. A loop that makes a table per iteration therefore holds every one of them until the typed call returns.
- **Impact.** A table pipeline (filter, with_columns, group_by, agg, sort) compiles as typed code and can feed a typed computation. Correctness: the table engine runs the same closures as from R, so results are the interpreter's. Analysis is unaffected. The typed `sales` benchmark does not use it (see "The typed `sales` benchmark is written over `Dict`").
- **Pinned by** `typed-table-values.scenario`.

## A serialized typed closure carries its signature

**Status:** built.

- **Previous shape.** A typed definition's signature (the type masks, the annotation texts and the formal names) lives in the session's registry (`interp::annotations`), keyed by the definition's formals, and the formals carry the `TYPED_DEFINITION` bit. Serialization wrote the bit, which is part of the object's flags, but not the registry entry. A typed function read back from a stream (an installed package's lazy-load database, `readRDS`, a saved workspace) was therefore still marked typed but had no signature: no argument check ran, a whole number passed for an `I64` parameter stayed a double, and the typed core compiled it for whatever the caller passed.
- **Target shape.** The registry stays the one source of truth in memory. The stream carries a copy: the writer (`libr::serialize`) gives a typed closure one extra first attribute, `annotations::STREAM_ATTRIBUTE`, holding the masks, texts and names, and the reader (`interp::serialize`) takes it off and registers it (`annotations::restore_signature`), so a closure in memory never has the attribute. Another R reads such a stream as a closure with one unknown attribute.
- **A typed function may call a sibling that is not loaded yet.** `typed::lower::resolve_function` forces a lazy-load promise of any package (recognized by `serialize::lazy_record_header`), not only base's, because such a promise only reads the function from the package database. Before, a typed function whose first call found a sibling still unloaded was refused for good.
- **Impact.** Installed typed packages check, convert and compile as the same code does in a script, which install-time compilation of typed closures needs: it reads the declared types. Pinned by the scenario `install-typed-package`.

## A record or tag that stays in its function has no box

**Status:** built.

- **Previous shape.** Every `Op::Construct` of a shape with fields allocated a box, also when the function only read fields back out of it. A first pass removed a box only when one instruction built it and every instruction on the way to a read was the single assignment of its local and dominated the read. A box built on both arms of an `if`, or rebuilt each time round a loop, kept its allocation, so `match (Str.to_i64(text)) { :Ok(n) => ...; :Err(_) => ... }` built `:Ok(n)` on one branch only to take it apart after the join.
- **What was structurally weak.** The dominance rule treated the IR's locals as if they were SSA values and then had to prove, with a dominator tree, that a read saw the one construction. Locals are variables, and a variable that holds a box can simply become several variables.
- **Target shape.** `typed::unbox` (`crates/typed/src/unbox.rs`) runs after lowering and before fusion and ownership insertion. A local that holds a record or tag becomes one `Int` local for the box's shape and one local per field that some instruction reads. A construction assigns the shape constant and copies the read fields, a tag without a payload (a static box, recognized by `shapes::constant_shape`) assigns its shape, a copy between two such locals copies every one of their variables, `Op::ShapeOf` reads the shape variable and `Op::Field` reads the field's variable. Every assignment of the old local assigns the new ones at the same place, so a read sees the same value on every path, with no dominance argument and no dominator tree. Because the pass runs before ownership insertion, no count operation ever exists for a removed box.
- **Groups.** Locals that are copied into each other form a group (a union-find over `Op::Copy`), which is replaced as a whole or not at all. A group keeps its boxes when a member is a parameter's box, is assigned by anything but a construction, a copy within the group or a static tag, or is used by anything but a shape read, a field read or a copy within the group: an argument, a list item, a field of a box that is itself kept, the result. It also keeps them when two reads of one field have different types, or when the constructions store values of different types that the box's own field type does not cover (`fits` allows equal types, a `Bool` stored as its logical, and a tag of a known union stored as a tag of any union).
- **A variable that only holds tags without payloads** is replaced too, although nothing allocates for it: a group is replaced when some instruction builds a box or a static tag for it. Its `match` then compares the shape number in a register instead of loading the shape out of the static box, which is what `orders` pays per item for its region tag. A group nothing builds for (a box a call returned, or the variables of an earlier round) is left alone.
- **A field only one shape has.** After a join of `:Ok(n)` and `:Err(e)`, the variable for `n` is not assigned on the `:Err` path. The read of it never runs for that shape, but liveness is not path-sensitive, so the ownership pass treats the variable as live there and may drop or copy it. That is sound because a local starts as zero and a zero string, list or box counts as nothing (`backend::dup` and `drop` test for it), and because a local that is live always holds either that zero or a value it owns.
- **Nested boxes** go in a later round: once the outer box is gone, the inner one's only uses are copies and reads. `MAX_ROUNDS` is 4. A field nobody reads gets no variable, so a box that was only stored in such a field loses its last use and goes too.
- **Impact.** Performance: `orders` went from about 19 to about 2.3 times its Rust twin's time on the iterate build with the first pass and the inlining below, and the group rule halved `team_scores` (two of its four boxes per row are parse results matched at once). Compile time: three linear walks and one rewrite per round, fewer than the dominator computation they replace. Pinned by `crates/typed/tests/test_unbox.rs` (hand-built IR for each rule), `typed-records-unboxed.scenario`, and the loop case of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` in `crates/jit/tests/test_tiers.rs`, which builds tags with string payloads on different branches and reads them after the join.

## A small typed function is lowered into its caller

**Status:** built.

- **Previous shape.** A call of a typed function from typed code was always `Op::CallTyped`, a native call with the callee compiled on its own. A record the callee returned crossed the call as a box, so nothing could remove it, and the caller's loop paid a call per item.
- **Target shape.** `Lowering::inline`, which already lowered local functions and function literals into their callers, takes the callee's storage rule as a parameter and also serves top-level typed functions (`crate::storage`, so an argument meets the callee's annotation exactly as at a separate call). The call site in `lower.rs` inlines when the callee is another function of the same environment, is not already being inlined (`inlining`), sits at most `MAX_TYPED_INLINE_DEPTH` (4) levels deep, takes exactly the arguments passed, cannot end invisibly (`tail_invisible`), and has a small body: at most `MAX_INLINE_CALLS` (48) calls and operators, no `return` and no call of itself (`small_body`). The function being compiled is the one exception to the last condition (next bullet).
- **A function that calls itself is lowered into itself, one level deep.** The same site takes the function being compiled as a callee when it is small by the same measure, with calls of itself allowed. The calls inside the inlined copy are real calls, because the function is then on `inlining`. A recursion therefore makes about half its calls, and each real call pays the frame, the depth guard and the status test once for up to three bodies. `fib` went from about 2.3 to about 2 times its Rust twin, whose compiler turns one of the two calls into a loop. A recursive function still does not inline into another function, which keeps a caller's size independent of how its callees recurse. One level and not more: each further level multiplies the body again, and compile time is part of the score. For the same reason only a function with at most two calls of itself (`MAX_SELF_CALLS`) qualifies: each call is one more copy of the body, and `expr_tree`'s `evaluate`, which recurses from five places, took about five times as long to compile for no measurable gain. The lowerer also counts the copies as it makes them (`self_inlines`), because a local function that holds the one call of the outer function is lowered at each of its own calls, and each of those would add a copy the count over the source cannot see.
- **Why at lowering and not on the IR.** The lowerer types values by flow, so an inlined body is specialized to the argument types at this site for free, and its constructions land in the caller where `typed::unbox` can remove them. An IR inliner would need the callee's IR for the same argument types first and a renaming of locals and blocks, for the same result.
- **A callee that cannot be inlined is called.** An inlined body is lowered under the caller's rules, which can refuse something a separate function may do (a body whose result is a call of R, for one). `lower::lower` therefore runs twice at most: when the first pass refuses inside an inlined typed callee, the second pass (`lower_calling`) lowers that callee as an `Op::CallTyped` and inlines the rest as before.
- **What a user can see.** `RY_TYPED_LOG` prints no "compiled for" line for a function that was only ever inlined.
- **Pinned by** `typed-record-list-fields.scenario` (the callee is compiled into its caller), `typed-calls-stay-native.scenario`, and the self-calling cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` in `crates/jit/tests/test_tiers.rs`.

## A typed loop over a held list counts a position and reads without a range check

**Status:** built.

- **Previous shape.** `for (x in xs)`, `List.walk`, `List.map` and the other inlined list functions kept an `I32` index, widened it per item and read through the checked item load.
- **Target shape.** `positions` and `count_up` in `crates/typed/src/lower.rs` open a loop whose `I64` counter runs from 1 to a length read once before the loop, and `next_position` leaves when the counter equals the end before it steps, so the step cannot overflow and needs no check. The body reads with `Op::ItemAt(list, counter)`, a load with no range check. That is sound because the loop reads a local the lowerer made for the loop (a copy that holds its own reference for the whole loop), not the user's variable, so the list's length cannot change while the loop runs.
- **`List.walk` over `List.range`** counts from the start to the end and builds no list.
- **Impact.** The array kernels' loops are a compare, a load and an add per item.

## A typed free calls a register-preserving entry

**Status:** built.

- **Previous shape.** Dropping the last reference called the free routine in the host's calling convention. The call is on a cold path, but the register allocator must assume a host call clobbers every caller-saved register, so each value live across a drop lived in a stack slot on the hot path too.
- **Target shape.** The backend compiles one thunk per free routine in Cranelift's preserve-all convention (`CallConv::Preserving` in `crates/ssa`) and calls that. The thunk saves what the host routine may clobber, so a loop's accumulator and counter stay in registers.
- **Why not inline the count test only.** The test was already inline; the spills came from the call's convention, not from its frequency.

## Fused list kernels are compiled per CPU feature level and clear no buffers

**Status:** built.

- **Previous shape.** `values::ry_count_true` and `values::ry_elementwise` were compiled once for baseline x86-64 (SSE2), which has no 64-bit integer compare and works on two doubles at a time. The arithmetic kernel's loops read slices built inside the function, which the compiler could not prove apart, and stayed scalar. Both kernels declared their chunk buffers as zeroed arrays, so each call cleared up to 36 KB before it read an item; in `log_rank`, which counts over 16,000 items a thousand times, the clearing was a sixth of the kernel's instructions.
- **Target shape.**
  - The count kernel's body is `#[inline(always)] count_true`, compiled twice: `count_true_avx2` carries `#[target_feature(enable = "avx2")]`, and `ry_count_true` picks it when `is_x86_feature_detected!("avx2")` says so. Its helpers (`operand`, `compare_lanes`) are `#[inline(always)]` too, because a function without the attribute is not compiled with the caller's features.
  - The arithmetic kernel calls one step function per program step and chunk through a pointer chosen once per call (`Step<T>`: `step_dbl_avx2` or `step_dbl_baseline`, and the same pair for `I64`). The slices are parameters of the step, so they are known not to overlap and every operator's loop is vector code, with the `NaN` or overflow test folded into the same loop. The last step of a program that gives a list writes into the list, with no copy.
  - `Rows` holds the chunk buffers uninitialized. A kernel fills the rows of its scalar inputs, clears only the rows its program's steps write, and reads no other row.
- **Why not `-C target-cpu`.** A released binary must run on any x86-64 machine, so the feature level is chosen at run time, per kernel. `crates/numeric/src/vector_math.rs` does the same.
- **Why two shapes of dispatch.** A call per step and chunk of 128 items costs nothing beside the chunk, and keeps one copy of the kernel's control code. The count kernel has the other shape, its whole body compiled twice; moving it to step functions is a cleanup, not a speed-up.
- **Impact.** On the iterate build, `log_rank` runs at about 3.5 times its Rust twin's time (from about 8) and `vector_arith` at about 1.9 (from about 2.6). The remaining cost is in the backlog ("Typed ry against Rust").
- **Pinned by** `fused_counts_follow_three_valued_logic` and `fused_arithmetic_agrees_with_one_item_at_a_time` in `crates/typed/tests/test_values.rs`, and the `typed-mask-counts` scenario.

## `Dict.update` probes once and builds no `Result`

**Status:** built.

- **Previous shape.** `Dict.update(dict, key, f)` lowered to a lookup that built `:Ok(value)` or `:Err(:Missing)` as a box, a call of `f` whose answer was a second box, and an insert or a removal that hashed and probed again. A loop that totals per key paid three hashes and two allocations per item.
- **Target shape.** The lowerer emits `Op::DictFind` (the key's position, or -1), branches on it, and lowers `f` once per branch when `f` is inline (a function literal, a local function or a small typed function): the found branch reads the value with `Op::DictAt`, applies `f` to `:Ok(value)` and stores with `Op::DictPut`, which writes at the known position, and the absent branch applies `f` to `:Err(:Missing)` and stores with `Op::DictInsert`. Each branch's `Result` is built and read inside one function, so `typed::unbox` removes it. A typed closure that is called, not inlined, keeps one joined lookup result and one call (`lower::dict_update`, `store_update`).
- **Why two copies of the callback.** One copy at a join would see a `Result` assigned on two paths, which the unboxing pass keeps as a box. Two copies cost compile time in proportion to the callback's size, which is small where it is inline at all (`MAX_INLINE_CALLS`).
- **`ry_dict_put` falls back to an insert** when the dict is shared, static or the position is -1, so `Op::DictPut` is correct wherever `Op::DictInsert` is.
- **Pinned by** `test_tiers::typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` (inline and called callbacks that insert, replace and remove) and `test_values`.

## The typed `sales` benchmark is written over `Dict`

**Status:** built.

- **Previous shape.** `sales.typed.R` ran a DataFrame pipeline (five eager table verbs and a group-by on a string column), while its Rust twin made one pass over three slices with a `HashMap`. The ratio between them measured the table engine and the R call protocol, not typed ry.
- **Target shape.** A typed benchmark program and its Rust twin do the same work in the same data model (the mandate's "written entirely in ry's own types"). `sales.typed.R` takes one `List` per column and totals revenue per region in a `Dict<Str, F64>` through `Dict.update`; `tools/bench/typed/sales.rs` does the same with a `HashMap<&str, f64>`. Both weight the regions in alphabetical order, so the checksum is the plain R program's.
- **What it still measures beyond the loop.** The three column lists are held by untyped code, so each call converts the `List<Str>` argument to a native list and scans the numeric ones (`typed::type_of`); the Rust twin borrows its slices. That boundary cost is part of the ratio on purpose, and `backlog.md` tracks removing it.

## Short strings are built and hashed as two words

**Status:** built.

- **Target shape.** A string of at most 15 bytes is two machine words with its bytes inline. `values::str_from_bytes` assembles those words from two fixed-size reads that overlap (two 8-byte reads for 8 to 15 bytes, two 4-byte reads for 4 to 7, three byte reads below), with no `memcpy` call, and `values::key_hash` hashes an inline string key by mixing its two words instead of walking its bytes. Equal inline strings have equal words, because the bytes past the length are zero, so the hash agrees with equality. The crate is little-endian only (`compile_error!` otherwise), which the word layout relies on.
- **A CHARSXP known to be UTF-8 is not validated again.** `typed::usable_string` accepts a string whose `VALID_UTF8` flag is set (interning sets it) and validates the bytes only without the flag.
- **Pinned by** `test_values::strings_switch_to_heap_past_fifteen_bytes`, which checks the exact words for every length from 0 to 15.

## The typed log reports each compilation's own time

**Status:** built.

- **Why.** Compile time is part of typed ry's performance bar (single-pass, Roc-class, far below `rustc`), so an optimization has to be weighed against what it costs to compile, and the cost has to be visible.
- **Target shape.** With `RY_TYPED_LOG=1`, `typed: compiled for [...] -> ... in N us` gives the time of that function alone: the time of a callee compiled during the caller's compilation is subtracted (`typed::NESTED_COMPILE`), and a compilation that ends in a refusal logs `typed: refused [...] after N us`, so the lines of a run sum to its total compile time. No clock is read without the log. The typed suite sums both kinds of line per program into `compile_seconds` beside `rustc`'s time for the twin (`tools/bench/bench.py`, `typed_log` and `RustRunner.compile_seconds`), and the benchmarks page shows them as the suite's "Compile time" table.

## One thread-local holds every typed allocation pool

**Status:** built.

- **Previous shape.** The typed runtime's free lists, large size classes and live-object counter were separate thread-locals, so one allocation read thread-local storage several times, and in `libR.so` each read is a call of `__tls_get_addr`.
- **Target shape.** `values::Pools` is one thread-local struct holding the free stacks, the large classes and `live`. `object_alloc` and `object_free` resolve it once and pass it down (`block_alloc_in`, `block_free_in`).
- **Impact.** One thread-local resolution per allocation or free. Its cost still shows in `team_scores` (`ry_box_new` is about 84 instructions), and the next step is in the backlog ("Typed ry against Rust").

## `Str.split_on` with a one-byte separator scans a word at a time

**Status:** built.

- **Target shape.** `ry_str_split` with a one-byte separator counts the separators first and cuts second, and both passes read the text eight bytes at a time: `values::separators_in_word` XORs the word with the separator repeated in every byte and marks the zero bytes with the carry-free test `!(((d & 0x7f..) + 0x7f..) | d | 0x7f..)`, which sets exactly the high bit of each matching byte. A tail shorter than eight bytes is read through `str_from_bytes`, never past the text. The result list is allocated once at its final length, and each field of at most 15 bytes is built inline by `str_from_bytes` with no allocation.
- **Why not `memchr`.** Fields in delimited records are short, so a call per separator costs more than the scan; the word loop has no call.
- **`box_free` walks set bits.** A box's `objects` and `strings` masks say which words hold a reference. `box_free` visits only those words (`trailing_zeros`, clear the lowest bit) instead of testing every word of the box.
- **Pinned by** the sweep in `test_values::string_functions_follow_r_and_the_standard_library` (every length from 0 to 40 with separators at eight strides, against Rust's `split`).

## A branch on a comparison of two doubles compares once

**Status:** built.

- **Previous shape.** `if (x > y)` on `F64` lowered to `Op::Compare`, which built a three-valued logical (an unordered test, a select of `NA`), and `Op::Truth`, which tested it for `NA` and again for truth. The loop body of a filter spent most of its instructions there.
- **Target shape.** `backend::emit_block` recognizes a block that ends in `Compare(op, Dbl, a, b)`, `Truth` of that result and a `Branch` on the truth, and emits the ordered comparison as the branch itself. An ordered comparison that holds proves neither operand is NaN, so the unordered test runs only on the path where it does not hold; `!=` holds for unordered operands, so there the unordered test comes first. The compare's and the truth's locals are still set (to the same 0 or 1), because later blocks may read them.
- **Compile cost.** A slice pattern on the block's last two instructions, no extra pass.
- **Pinned by** `test_tiers::typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` (all six operators on both sides, and `0 / 0` compared with `!=` and `<`, where the interpreter answers).

## `List.count_if` with a comparison predicate counts over the whole list

**Status:** built.

- **Previous shape.** `List.count_if(xs, function(item: F64) -> Bool item > 0.5)` lowered to a loop that called the inlined predicate per item, with an interrupt poll, a data-pointer reload and an `NA` test each time, while `List.count_if(xs > 0.5, function(b) b)` ran the chunked count kernel. The natural spelling was the slow one.
- **Target shape.** In `Lowerer::walk`, a `count_if` over a `List<F64>` whose predicate is an inline one-parameter function that only compares its item with numbers (`Lowerer::compares_item`: `== != < <= > >=` with the item on one side and a double literal or an `F64` local on the other, joined by `&`, `|`, `!` and parentheses) lowers the predicate's body once with the parameter bound to the list. The body then is the list comparison, and the count is `Op::CountTrue` over its mask, which `typed::fusion` turns into one kernel call. Any other predicate keeps the per-item loop.
- **Why only `List<F64>` and only comparisons.** Binding the list to the item's name is sound only where every operation on the item means the same elementwise. Comparisons and the logical connectives do; arithmetic on an `I64` item is checked per item and may fail, and a predicate with a call or a `match` has no list form. An `I64` parameter over `List<I64>` is left out because the declared-`I64` lowering of the body differs from a list comparison; add it when a program needs it.
- **The count kernel tallies a single comparison directly.** `values::count_true` with a one-step program adds the comparison's answers as it compares (`tally_compared`), with no lane written per item, and the general path tallies a chunk's lanes in a byte (a chunk is 128 items, a true lane is 2), so the tally adds whole vectors of lanes.
- **Pinned by** `test_tiers::typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` (a literal, a local and both operand orders, connectives, a named local predicate, a shadowed name, the empty list, and a computing predicate on the loop path) and `test_values::fused_counts_follow_three_valued_logic`.

## The typed `array_filter` benchmark counts with `List.count_if`

**Status:** built.

- **Why.** A typed benchmark program and its Rust twin are both written the way the language's users would write them and do the same work. The Rust twin counts with `filter` and `count`, which `rustc` vectorizes; a typed element loop with a float accumulator measured the float-add chain against that, not the language. `array_filter.typed.R` counts with `List.count_if` and a comparison predicate, the same shape as the twin.
- **What an element loop costs.** A typed loop that tests each item and adds to an `F64` total is bound by the add chain and the per-item poll, about three times the vectorized count. Typed ry has no autovectorizer and does not get one (compile time is single-pass), so whole-list operations are the fast spelling, as in the standard library's other list functions.

## Shapes are confirmed after lowering, and machine code is generated once

**Status:** built.

- **Previous shape.** `typed::compile` ran the whole pipeline (lowering, unboxing, fusion, ownership, Cranelift) and confirmed the shapes the code constructs afterwards. A function that constructs a shape nobody confirmed yet and also matches over any tag then held a stale epoch, so the pipeline ran a second time and the first machine code was thrown away. Every program that builds its own records or tags and reads a tag out of a record paid a double compilation (`orders`, `team_scores`, `expr_tree`).
- **Target shape.** `compile` lowers (`lower_settled`), confirms `Lowered::constructed`, and lowers again only if a `match` over any tag saw an older epoch. `generate` runs the rest of the pipeline once, on the lowering that settled. A lowering is a small part of a compilation, so the repeat is cheap.
- **Confirming before the code exists is safe.** A confirmed shape only adds a case to matches over any tag. If the backend then refuses the function, later matches test for a shape nothing constructs, which costs a comparison and changes no answer.
- **Impact.** `orders` compiles in about 1.5 ms where it took 2.4 ms, and every typed benchmark program generates code once per compiled function (`RY_SSA_DUMP=1` prints one `function Tail` per `typed: compiled` line).

## `List.sum` adds doubles in order in double precision

**Status:** built.

- **Previous shape.** `List.sum` over `F64` accumulated in x87 long double, like R's `sum()`, in the interpreter (`standard_library::list_sum`) and in compiled code (`Reduce::Sum`, and the total of a fused `Op::Elementwise`). The rule existed so that `List.sum(xs)` and `sum(xs)` gave the same number. It tied a ry function to an R kernel's arithmetic, it kept the sum on the x87 stack, where it cannot vectorize and costs several cycles of latency per item, and it made the typed `array_map` benchmark pay for precision its Rust twin does not have.
- **Chosen shape.** `List.sum` over `F64` is the left-to-right sum in double precision, starting from zero, as a loop with `+` gives. The interpreter folds with `+`, compiled code calls `values::ry_list_sum_dbl` (`Reduce::ListSum`), and a fused elementwise total adds each chunk's items onto one `f64` total in order (`values::add_in_order`), so all three produce the same bits. R's `sum()` (`Reduce::Sum`, `values::ry_sum_dbl`) keeps the long double fold, because GNU R defines it.
- **Why in order and not a vectorized order.** A fixed multi-lane order would be faster again, but the interpreter, the kernel's chunked total and a user's own loop would each have to implement the same lane layout to agree. In-order addition is the one rule every path gives without trying, and it is what `xs.iter().sum()` gives in Rust and `List.sum` in Roc.
- **Impact.** `List.sum(xs)` can differ from `sum(xs)` in the last digits (`List.sum([1, 1e-16, 1e-16])` is `1`, where `sum()` gives the next double), which the reference states. `test_standard_library` pins the difference and `test_tiers` pins that compiled and interpreted sums agree bit for bit. `array_map` falls under the bar of twice its Rust twin.

## Counts that share a list share one kernel pass

**Status:** built, except the "Not done" bullet on range facts, which "Whole-number ranges decide which checks typed code needs" supersedes.

- **Previous shape.** `Op::CountTrue` had one destination, so each `List.count_if` ran its own pass of `values::ry_count_true` ("A count of a logical list fuses with the comparisons that built it, after lowering" above). The log-rank statistic asks five counts per event time over the same three lists (`time >= t`, `time >= t & treated`, `time == t & event`, that with `& treated`, and `event`), so it read the lists five times and compared `time >= t` twice, where its Rust twin makes one pass with five counters.
- **Chosen shape.** `Op::CountTrue { program, inputs, counts }` carries several counted operands, each paired with the `I64` local that receives it, and the instruction has no `dest`. `Inst::assigned` is the one place that says which locals an instruction writes, and every pass that asks "does this assign that local" goes through it. The kernel tallies each counted row of a chunk, and a counted operand may be a step or a logical list input. At most `values::COUNTED_LISTS` (8) counts share a pass.
- **The merge rule (`fusion::merge_counts`).** After each count has absorbed its own comparisons, a count merges into the nearest earlier count of its straight-line run (its block and the single-predecessor blocks that jump into it) when all of these hold: no `CallR` or `CallTyped` lies between the two, nothing from the earlier count on assigns an input or a result of the later count, nothing between them reads a result of the later count, the two counts read at least one list in common, and the merged program fits the kernel's step, input and count limits. The later count's steps are appended to the earlier program, and a step identical to one already there is reused, so a shared comparison runs once.
- **Why the counts must share a list.** A pass fails unless all its list inputs have one length. Two counts over unrelated lists (`List.count_if(wins, ...)` then `List.count_if(losses, ...)`) each succeed alone whatever the lengths, so one pass over both would give up, and the interpreter would rerun, every call whose lists differ in length. A shared list ties the lengths together: the merged pass then fails only where one of the two counts would have failed alone.
- **Why a call keeps two counts apart.** A count fails on `NA`, and a failing instruction gives up the whole call so the interpreter can rerun it. Moving the later count before an R call would let it fail after that call was skipped the first time and then run it twice. Calls are the only effect between counts that a rerun repeats.
- **Why the later count moves up, not the earlier one down.** The earlier count's results may be read between the two (`n1 <- count(a); x <- n1 + 1; n2 <- count(b)`), and moving the later count up needs only its own inputs to be unchanged over the gap, which the same scan establishes.
- **Impact.** Performance: typed `log_rank` makes one pass over its lists per event time. Compile time: one more linear walk over the instructions of functions that contain a count. Simplicity: `dest` no longer says everything an instruction writes, which is why `Inst::assigned` exists.
- **Not done: range facts for `I64` subtraction.** `fib`'s `n - 1` and `n - 2` cannot overflow after `n < 2` is false. Emitting them without the overflow check changed `fib`'s time by less than the noise between runs, so no range pass was added.
- **Pinned by** `crates/typed/tests/test_fusion.rs` (which counts share a pass, and each rule that keeps two apart), `fused_counts_follow_three_valued_logic` in `crates/typed/tests/test_values.rs` (several counts against a model of three-valued logic), and the log-rank cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` in `crates/jit/tests/test_tiers.rs`.

## Parsing a number is one instruction with two results

**Status:** built.

- **Previous shape.** `Str.to_i64` and `Str.to_f64` lowered to `Op::Parses`, a test that ran the parser and threw the number away, and then on the valid branch `Op::Parse`, which ran the parser again. Every string was parsed twice.
- **Chosen shape.** `Op::Parse { string, parsed }` assigns the number to its destination and whether the string parsed to `parsed`, from one call of `values::ry_str_to_i64` or `ry_str_to_dbl`. The destination is zero when the string does not parse, and the instruction cannot fail. It is the second instruction after `Op::CountTrue` that assigns a local besides `dest`, which `Inst::assigned` reports.
- **Impact.** One parse per string. With the box of `:Ok(number)` gone (see "A record or tag that stays in its function has no box"), `match (Str.to_i64(text))` costs the parser call and a branch.

## An `I64` remainder or quotient by a positive constant needs no checks

**Status:** built.

- **Previous shape.** `backend::i64_arith` emitted the same code for every `%%` and `%/%`: a test for a zero divisor, a select that replaces a divisor of -1, the division, and a floor fix that compares the remainder with zero, xors it with the divisor, tests the sign and selects. With the divisor a constant, Cranelift folded the two tests and turned the division into a multiplication, but the floor fix stayed: ten instructions per operator, and `orders` runs four per item.
- **Target shape.** When the divisor is a positive constant (`ssa::Builder::constant` reads it off the value's defining `iconst`), the zero and -1 cases cannot happen, and a truncated remainder is negative exactly when the floored one differs. So `%%` is the remainder plus the divisor masked by the remainder's sign (shift, and, add), and `%/%` is the quotient plus that sign. A power of two needs no division at all: the low bits are the floored remainder of a negative number too, and an arithmetic shift rounds down.
- **Why in the backend.** The typed IR's locals are variables, so it cannot say an operand is constant, while the SSA builder knows the definition of every value. A constant assigned in another block than the operator reads is a block parameter until the blocks are sealed and takes the general path.
- **Impact.** `orders` went from about 114 to about 89 instructions per item (callgrind, release build), and from about 2.4 to about 1.8 times its Rust twin. Compile time falls, because less code is emitted. Pinned by the constant-divisor cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` in `crates/jit/tests/test_tiers.rs`, which check negative dividends, `Num.min_i64` and a zero or negative constant against the interpreter.

## A split list that is only read at constant positions is never built

**Status:** built.

- **Previous shape.** `parts <- Str.split_on(row, ","); parts[[2]]` allocated a list and one string per piece, read some of them, and freed everything. In `team_scores` that was about 690 of about 1,750 instructions per row (callgrind): two passes over the text, the list, a string per piece, and the free.
- **Chosen shape.** `unbox::replace_split_lists` runs before `unbox::unbox` and replaces a group of list variables with variables for the string, the number of pieces, and the start and end of each piece that some read names. `Op::SplitBounds` assigns them from one scan (`values::ry_str_split_bounds`), which stops after the last piece any read names. A read becomes `Op::Piece`, which fails when the string has fewer pieces, exactly where the list read failed, and otherwise cuts its piece out of the string (`values::ry_str_piece`). No list exists, and a piece no path reads is never made.
- **Which lists.** A group of `List<Str>` variables joined by copies, where every assignment is a split or a copy within the group and every use is such a copy or an `Op::Get` whose index is a local that one `I64` constant assigns, from 1 to 32. A list that is returned, measured with `List.len`, passed on, or read at a computed position keeps its list.
- **Why the variables hold the string.** The IR's locals are variables, so the string a split read may be assigned again before a piece is read. The split copies it into a variable of its own at the place the list was assigned, which makes every read see the string of the split it reads, on every path and around loops, by the same argument as for a box's fields.
- **Why offsets and a cut at the read, not the pieces as strings.** The twin in Rust does the same: `row.split(',')` yields slices. A row that is rejected after its second field never pays for the other pieces. The cost is that a read executed many times cuts its piece each time, where a list read would copy a reference. Splitting inside the loop and reading each piece once is the common shape, so the pass takes that trade.
- **Impact.** `team_scores` went from about 1,750 to about 1,420 instructions per row, and from about 2.9 to about 2.2 times its Rust twin (release build, a shared machine). `Op::SplitBounds` is the third instruction that assigns locals besides `dest`, which `Inst::assigned` reports.
- **Pinned by** `crates/typed/tests/test_unbox.rs` (which lists are replaced and which uses keep one), `check_split_bounds` in `crates/typed/tests/test_values.rs` (the offsets against Rust's `split` for every limit, with one-byte, wide and empty separators), and the split cases of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` in `crates/jit/tests/test_tiers.rs` (a read past the last piece, a string assigned again, and a list assigned again on a branch and in a loop).

## A loop that fills a list is fused with the loop that reads it

**Status:** built.

- **Previous shape.** `List.map`, `List.keep_oks`, `List.walk` and a `for` over a list each lowered to a counted loop of their own, and each stage handed the next a list. In `team_scores` (`parsed <- List.map(rows, parse_row)`, `people <- List.keep_oks(parsed, ...)`, `List.walk(people, ...)`) a row allocated a `Result` box and a record box, pushed them on two lists and freed them, although every item was read exactly once by the next loop. `fusion::fuse` only fuses element-wise arithmetic over numeric lists and does not see loops.
- **Chosen shape.** `loops::fuse_loops` (`crates/typed/src/loops.rs`) runs on the typed IR and moves the body of the reading loop (the consumer) into the loop that fills the list (the producer), at the place of the `Push` or `Set`, which it deletes. The item goes from one body to the next in a variable, the list is never made, and a counter stands in for its length, so a later `List.len` still has its answer. A three-stage pipeline fuses in two rounds into one loop. Once no list holds them, the boxes of the items never leave the function, and `unbox::unbox` replaces them with their fields.
- **Which lists.** The module documentation lists the conditions. In short: one counted loop fills the list once per iteration (a `Push` onto a list that starts empty, or a `Set` at the loop's own counter into a list as long as the one the loop counts over); another counted loop reads it from the first item to the last and leaves only at its guard or latch; nothing else reads the list but its length; the consumer follows the producer on a straight path whose only branches give up the call; neither loop calls a function or returns; and neither loop assigns a variable the other reads or assigns.
- **Why running the consumer's body earlier is sound.** Before the fusion, every producer iteration ran before any consumer iteration. After it they alternate. Without calls, a body's only effects are on locals and on the heap values those locals own, and the interference test (`Bits::intersects` over what each loop reads and assigns) rules out any shared variable. What remains is which failure gives up the call when both loops can fail, and `Terminator::Fail` carries no payload: every failure gives the call to the interpreter the same way, which then runs the function from the start. The one difference the pass accepts is a consumer body that never ends: unfused, a later item that fails the producer would have given up the call before the consumer started.
- **Why making the list must not fail.** Deleting the list deletes its creation. `Op::EmptyList` with a negative capacity fails, so the capacity must be known not to be negative (`never_negative`: a non-negative constant, a length or a count). The instruction that computes the capacity stays, with its own failure.
- **The store's own failure stays.** A ry list of doubles holds no `NaN`, and `Op::Set` into one gives up the call for a `NaN` or `NA` item (`backend.rs`, the `Set` arm), which is what makes `List.map(xs, \(x) x / x)` over a zero end in the interpreter's "a list cannot hold NA". The fused loop keeps that test as an `Op::IsNan` and a branch to a failing block at the place of the store. A list of R integers or logicals filled by `Set` has the same failure and no such test in the IR, so it is not fused (`List.map` refuses to build one anyway). `Op::Push` has no such test in either form.
- **Where the list may be made.** A `for` over `List.range(1, n)` puts its range check between the code before the loop and the loop's guard, so a list the user makes before the `for` is a block ahead of the guard. `runs_into` accepts a creation on the single-predecessor path into the producer's guard, up to `MAX_BETWEEN` blocks. A list filled by `Set` must be made in the guard itself, because its length is the loop's.
- **Why before `unbox` and ownership.** Before the reference counts are inserted a list is a value, `Push` and `Set` return a new one, and no `Dup` or `Drop` has to be moved or paired again. Running before `unbox` is the point: the boxes become removable only after the lists that held them are gone.
- **Three changes the fusion needs to pay off.** (1) `List.map(rows, parse_row)` with a named typed function inlines the function at `apply` under the rule a direct call uses (`lower::typed_function`), where it was an `Op::CallTyped` per item, and a loop with a call never fuses. (2) `unbox::remove_unread` runs after `unbox` and removes instructions whose result nothing reads and that cannot fail: a record that became its fields leaves the fields no read asks for behind, and for a string that was an allocation per row. (3) `Op::Piece` at index 1 cannot fail (`ir::Op::can_fail`), because a string always has a first piece, so an unread first piece is removable.
- **Known limits** (in `backlog.md`): a third stage after two `Set`-filled lists (map, map, consumer) fuses only the first two, loops that share an assigned variable stay apart, and a loop with a call of R or of a typed function that was not inlined never fuses.
- **Impact.** `team_scores` went from about 1,420 to about 890 instructions per row (callgrind), and from about 2.2 to about 1.2 times its Rust twin (release build, a shared machine). Its loop allocates no list and no box; what remains is the scan for the separators (about 180), generated code (about 165), cutting the pieces that are parsed (about 150), the two number parsers (about 165) and the dictionary update (about 135). The suite's total typed compile time stayed within the spread of single runs (about 37 to 39 ms for the seventeen programs), because the pass looks only at functions with two counted loops and matches a fixed shape.
- **Pinned by** `crates/typed/tests/test_loops.rs` (each condition as a pipeline that fuses or keeps its list, and a fuzz that runs random pipelines fused and unfused through an IR evaluator and compares results and failures; `FUZZ_SEED=<n> cargo test -p typed --test test_loops` replays one), `an_unread_value_goes_with_what_only_it_read_unless_computing_it_can_fail` in `crates/typed/tests/test_unbox.rs`, and the pipeline cases of `typed_core_keeps_tier_zero_semantics_for_records_tags_and_text` in `crates/jit/tests/test_tiers.rs` (rows dropped, none kept, a row missing a piece, an empty list, a `for` that appends and breaks, an overflowing item), which compare against the interpreter.

## Whole-number ranges decide which checks typed code needs

**Status:** built.

- **Previous shape.** The backend emitted each operator's checks from the operator alone. Every `I64` `+`, `-` and `*` tested for overflow, a `%%` or `%/%` by a positive constant corrected for a negative dividend, and `Op::Truth` and `Op::ToDbl` tested an integer or logical for `NA`, also where the operands could not make the check fire: `i %% 7 + 1` for a counter `i` that starts at 1, `n - 1` behind `if (n < 2)`, the conversion of a `for` counter to a double. The only fact the backend had was whether a divisor's SSA value was a constant.
- **Chosen shape.** `ranges::ranges` (`crates/typed/src/ranges.rs`) computes one interval per whole-number local of the typed IR, and `backend::compile` reads it through `Ranges::of(block, local)`. An `I64` `+ - *` whose exact result interval fits in `i64` is a plain add, subtract or multiply. A `%%` or `%/%` whose divisor is known positive has no zero and -1 tests, and no floor correction when the dividend is known not negative. `Op::Truth` and `Op::ToDbl` skip the `NA` test for an operand whose range starts above `NA`.
- **How a range is found.** The locals are variables, so a range covers every assignment of the local in the function. The ranges start empty and grow until nothing changes, and a bound that is still growing after three rounds jumps to the limit of its type, which bounds the work. Up to three descending rounds then recompute every range from the ranges found, because the jump also widens values computed from a widened one: `i %% 7` is at most 6 whatever the counter's upper end is. A local that some path reads before assigning has 0 in its range, because the machine code starts every non-parameter local at zero.
- **Tests narrow a range for the code they guard.** A block that ends in a branch on a comparison of two `I64` locals gives each side of the branch a tighter range for both, in every block the branch target dominates. It counts only when the branch is the only way into its target and the condition and both operands are assigned at most once, in a place that runs before the comparison every time (a parameter, earlier in the same block, or in a dominating block). A local assigned once changes only when that assignment runs again, and every way from there back into a dominated block leads through the comparison, so the test still holds wherever the narrowed range is read.
- **What a range must cover.** A wrong range is a miscompile, so each transfer function follows what the backend emits for that instruction and destination type: a length written to an integer is truncated and keeps the type's whole range, an instruction that assigns locals besides `dest` gives them their type's range, an operator that gives up on overflow has its result clamped to the type, and a logical or a `Bool` is not taken to hold only 0 and 1, because a logical vector filled by a package's C code can hold another true value and `Op::Truth` passes it on unchanged. The analysis also relies on two contracts of the IR: `Op::Step` is only reached for a counter that has not reached its end, so it does not overflow, and a `Bool` is never `NA`.
- **Why not SSA value ranges in `crates/ssa`.** The typed IR knows the types (`Int` with its `NA`, `Bool`, `I64`) and the operators whose checks are at stake, and the backend picks the instruction sequence while it emits. Ranges on SSA values would have to recover both from the emitted code.
- **A cost the change exposes.** Cranelift places a pure value at its first use in each branch of the dominator tree. With its overflow test gone, `i %% 7 + 1` has no use in the block that assigns it, so `orders` computes it in one arm of the next branch and again at the join. The checks removed outweigh the recomputation.
- **Impact.** `orders` runs about 76 instructions per order line, from about 88, and `fib` about 7% fewer (callgrind, iterate build). The times on a release build moved less than the spread between runs (`orders` stays near 1.8 times its Rust twin, `fib` near 1.7). The likely reason, not measured, is that the removed tests were branches the processor predicts and runs beside the arithmetic. The suite's total typed compile time stayed within the spread of single runs, because the analysis is a few linear walks over the function. The facts are what later steps need: a counter known not negative, a value known not `NA`.
- **Pinned by** `crates/typed/tests/test_ranges.rs` (each rule as a small function, and a fuzz that runs random functions through an IR evaluator and checks every value read lies in its range; `FUZZ_SEED=<n> cargo test -p typed --test test_ranges` replays one) and the range cases of `typed_core_keeps_tier_zero_semantics_for_i64_and_list_arithmetic` in `crates/jit/tests/test_tiers.rs` (negative dividends, `Num.min_i64` and `Num.max_i64` behind a test, a variable assigned again after a test, a test in a loop), which compare against the interpreter.
