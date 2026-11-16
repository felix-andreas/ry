#!/usr/bin/env python3
"""Keeps the agent knowledge base in `.agents/memory/` navigable.

    python3 tools/memory.py split   # move entries from decisions.md into decisions/<area>.md
    python3 tools/memory.py index   # regenerate decisions/index.md
    python3 tools/memory.py check   # fail on a legacy decisions.md, a stale index or an over-budget file

`split` is idempotent and append-safe: an entry whose heading already lives in an area file is
updated in place when its body differs and skipped otherwise, and every other entry is appended to
the area its heading and body point at. Re-running it after `git checkout origin/main --
.agents/memory/decisions.md` carries over whatever other sessions appended to the legacy file.
"""

import collections
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MEMORY = os.path.join(ROOT, ".agents", "memory")
LEGACY = os.path.join(MEMORY, "decisions.md")
AREA_DIR = os.path.join(MEMORY, "decisions")
INDEX = os.path.join(AREA_DIR, "index.md")

# Byte budgets, a ratchet about 5% above the sizes these files had when the budgets were set. A
# later change tightens them; a file over budget is pruned or moved to the right horizon, not
# given more room.
BUDGETS = {
    "AGENTS.md": 40_000,
    ".agents/memory/MEMORY.md": 65_000,
    ".agents/memory/backlog.md": 483_000,
    ".agents/memory/worklog.md": 141_000,
    ".agents/memory/decisions/syntax-semantics.md": 262_000,
    ".agents/memory/decisions/ide-server.md": 60_000,
    ".agents/memory/decisions/interp.md": 120_000,
    ".agents/memory/decisions/builtins.md": 194_000,
    ".agents/memory/decisions/r-compat.md": 57_000,
    ".agents/memory/decisions/jit-ssa.md": 178_000,
    ".agents/memory/decisions/heap-gc.md": 157_000,
    ".agents/memory/decisions/libr-rcmd.md": 140_000,
    ".agents/memory/decisions/typed-stdlib.md": 221_000,
    ".agents/memory/decisions/frames-table.md": 89_000,
    ".agents/memory/decisions/tooling-bench-ci.md": 73_000,
    ".agents/memory/decisions/docs-process.md": 30_000,
    ".agents/orchestrator.md": 30_000,
}
# Longest line each file may hold. A log that is meant to be one line per entry stays that way.
LINE_LIMITS = {
    ".agents/orchestrator.md": 2048,
}

# Area file name, title, and what belongs there. The order is the index's order.
AREAS = [
    ("syntax-semantics", "Syntax and semantics",
     "The parser and syntax tree, the salsa analysis core, name resolution, and the type checker."),
    ("ide-server", "Editor and language server",
     "The language server, editor features, the formatter, lints, the check command's reporting, "
     "and the console and debugger."),
    ("interp", "Interpreter",
     "The evaluator: calls, promises, frames, environments, lookups, dispatch and conditions."),
    ("builtins", "Builtins and base R",
     "Base R's builtins and native kernels, numerics, regular expressions, connections, the default "
     "packages, and package code that runs natively."),
    ("r-compat", "R compatibility",
     "Matching GNU R's observable behavior: the oracle, corpus package tests, and R semantics details."),
    ("jit-ssa", "JIT and SSA",
     "Tier 0, the Cranelift tier, the shared SSA IR, deoptimization, and what compiles when."),
    ("heap-gc", "Heap and collector",
     "The object model, allocation, the garbage collector, and start-up and code images."),
    ("libr-rcmd", "C API, R home and packages",
     "The `libR` C API, `R CMD`, package installation, the bundled base R, and the loader behind "
     "`ry load`, `ry test` and `ry document`."),
    ("typed-stdlib", "Typed ry and the standard library",
     "Typed closures and their native compilation, records, tuples and tags, and `std`."),
    ("frames-table", "DataFrame and the table engine",
     "The built-in DataFrame, its kernels, CSV reading, and the data.table boundary."),
    ("tooling-bench-ci", "Build, benchmarks and CI",
     "Build profiles, releases, platforms, the dev shell, CI, fuzzing harnesses and benchmark suites."),
    ("docs-process", "Docs, process and positioning",
     "The docs site, licensing, positioning, and how agents record and organize knowledge."),
]
AREA_NAMES = [name for name, _, _ in AREAS]

# Record titles whose sections only make sense under them. Their sections' headings get the label
# as a prefix when split. Every other record title only grouped whatever was appended below it.
FOLD_RECORD = {
    "Decision record: the beta semantics": "Beta semantics",
    "Decision record: the shipping stack is a hand-written parser, a lossless tree, and salsa":
        "Shipping stack",
}
RECORD_PREFIX = "Decision record: "

# Classification overrides by heading (after folding), for entries the keyword scores misplace or
# score with low confidence. Pinned by reading each entry.
OVERRIDES = {
    "The beta semantics":
        "syntax-semantics",
    "Beta semantics: A reported column counts characters, and caret art counts terminal cells":
        "ide-server",
    "Beta semantics: Config subsystem":
        "ide-server",
    "Shipping stack: Item granularity and item identity":
        "syntax-semantics",
    "Shipping stack: The inline annotation syntax is a superset of R":
        "syntax-semantics",
    "Shipping stack: Inline type definitions, coercions and nominal constructors":
        "syntax-semantics",
    "Shipping stack: Corpus mechanics":
        "syntax-semantics",
    "Script frame semantics":
        "syntax-semantics",
    "Capture liveness is frame-scoped":
        "syntax-semantics",
    "Conditional slots type, export edges generalize, and `missing()` flows through the environment":
        "syntax-semantics",
    "Data.table awareness":
        "syntax-semantics",
    "The Zed extension versions on its own line":
        "ide-server",
    "One source of truth for where an item spells its name":
        "syntax-semantics",
    "Licensing":
        "docs-process",
    "The runtime's object model, collector and native boundary":
        "heap-gc",
    "Frames as runtime values":
        "frames-table",
    "Inline annotations at run time":
        "typed-stdlib",
    "The standard library runs only on ry's runtime":
        "typed-stdlib",
    "parallel's fork routines":
        "builtins",
    "One serializer writer, in libr":
        "libr-rcmd",
    "Call checking follows R's lazy argument and matching rules":
        "syntax-semantics",
    "Called names are looked up as functions":
        "syntax-semantics",
    "testthat files mirror testthat's environments":
        "syntax-semantics",
    "useDynLib native symbols resolve only as a native routine's name":
        "syntax-semantics",
    "`object-systems = false` stops class definitions in project code at run time":
        "interp",
    "A strict library file's top level holds only definitions, and purity is declared":
        "syntax-semantics",
    "Editor features for ry's own syntax":
        "ide-server",
    "What keeps a dropped object alive: registers, handles, caches and dying finalizers":
        "heap-gc",
    "ALTREP elements go through the class methods everywhere":
        "libr-rcmd",
    "Smaller runtime choices":
        "interp",
    "Array storage stays row-major; record Arrays go columnar only with JIT field fusion":
        "typed-stdlib",
    "Editor responsiveness at 300,000 lines":
        "ide-server",
    "A parallel startup warm-up in the server":
        "ide-server",
    "ry run and ry R: the command chooses the runtime":
        "libr-rcmd",
    "Data-oriented design and explicit allocators":
        "docs-process",
    "`ry check` releases its trees once, and memoized records are compact":
        "syntax-semantics",
    "`just ry` builds ry's runtime; ry-lang does not depend on libr":
        "tooling-bench-ci",
    "The engine's workers return freed memory at large frame boundaries":
        "frames-table",
    "A filtered `:=` reads a row index and writes in place":
        "frames-table",
    "In-place updates through nested targets":
        "typed-stdlib",
    "Moving a collection into a helper's parameter":
        "typed-stdlib",
    "A function literal in a call argument is typed in context":
        "syntax-semantics",
    "A formula is the nominal `formula`, and its operands are not judged":
        "syntax-semantics",
    "`$`, `[` and `[[` dispatch to a class's subset method":
        "syntax-semantics",
    "A new top-level name costs one project fold":
        "syntax-semantics",
    "The C API draws from the interpreter's generator":
        "libr-rcmd",
    "Native code reads an environment as a cell":
        "libr-rcmd",
    "Typed checking costs per name, not per file":
        "syntax-semantics",
    "The debugger lives inside the interpreter":
        "ide-server",
    "A conditional write of a formal covers the paths that run it":
        "syntax-semantics",
    "`<<-` writes flow forward, and a NULL-initialized cache reads its own first write":
        "syntax-semantics",
    "A call on a parameter whose arity the body tests is unchecked, and a later sibling does not force":
        "syntax-semantics",
    "Native scalar functions: frameless calls with checks proven at compile time":
        "jit-ssa",
    "`ry check` keeps an on-disk record of its findings":
        "ide-server",
    "Lazy definitions keep source references":
        "libr-rcmd",
    "The data-table lint is opt-in and marks only constructors":
        "ide-server",
    "The Rust table benchmark runs on libR's allocator and the same lazy plans as the R path":
        "frames-table",
    "Runtime semantics settled by the corpus package tests":
        "r-compat",
    "utils' sockets, internal downloads and allocation profiling":
        "builtins",
    "One session temporary directory, removed at exit":
        "builtins",
    "Base R kernels: position tables, a most significant digit radix order, and a per-call string memo":
        "builtins",
    "Compiled function lookups also check the activation's own frame":
        "jit-ssa",
    "A parameter and its named arguments are one editor binding":
        "ide-server",
    "The item check's working maps live in a pooled bump arena":
        "syntax-semantics",
    "The strict load check shares the check record":
        "libr-rcmd",
    "The bench templates are inline-typed":
        "tooling-bench-ci",
    "Packages without stubs are read from the R library by the host, lazily":
        "syntax-semantics",
    "One-shot checks record coarse dependencies":
        "syntax-semantics",
    "R's dynamic help server runs on the interpreter thread, only while R waits":
        "ide-server",
    "Inferred signatures carry body facts that a call reads":
        "syntax-semantics",
    "`Reduce` folds a named function instead of matching `fn(U, T) -> U`":
        "syntax-semantics",
    "A C API argument that may be a C `NULL` is typed `Option<Sexp>`":
        "libr-rcmd",
    "Source edits arrive by OS notification, ordered by a barrier file":
        "libr-rcmd",
    "Package-test compatibility details that look optional but are not":
        "r-compat",
    "Completion, signature help and import fixes reach untyped and installed code":
        "ide-server",
    "An integer and a double joined stay unboxed":
        "jit-ssa",
    "Base kernels pick their data structure by the value range":
        "builtins",
    "Runtime error checks follow GNU R's order and blame":
        "r-compat",
    "`as.character` of a long number vector is deferred":
        "builtins",
    "Frames released at return":
        "heap-gc",
    "Start-up memory: release relocation tables and preflight code pages":
        "heap-gc",
    "`format()` of numbers streams into the result":
        "builtins",
    "A masked argument is its own scope, and the base masking family is typed":
        "syntax-semantics",
    "`length(x)` guards drop `NULL`":
        "syntax-semantics",
    "A numeric binding yields to a matrix or a generic argument":
        "syntax-semantics",
    "`ry test --watch` selects callers through S3 dispatch":
        "libr-rcmd",
    "Any save refreshes every open document's diagnostics":
        "ide-server",
    "A call-hierarchy item answers only in its own document":
        "ide-server",
    "Fast-path prologues return through an op, not an unwind":
        "jit-ssa",
    "`[[` by name on a named atomic vector is the element, not `T | NULL`":
        "syntax-semantics",
    "The checker's per-item facts are columns and bitsets indexed by dense id":
        "syntax-semantics",
    "An item's syntax has a stamp, so an edit lowers only the items it changed":
        "syntax-semantics",
    "The typed code file is position-independent code whose offset table is the per-process table":
        "typed-stdlib",
    "The methods package's core runs natively behind hash-guarded prologues":
        "builtins",
    "Bytecode is build output, not a cache":
        "jit-ssa",
    "Tier 0 runs on facts fixed before the code runs":
        "jit-ssa",
    "Step 2 of the untyped roadmap: slot frames rejected on measurement, setters compiled, step 3 pulled forward":
        "jit-ssa",
    "Single-thread first, parallel-ready architecture (user directive)":
        "interp",
    "The effect-free fact is stored per code object and resolved per enclosure":
        "jit-ssa",
    "Argument-match caches live in the callee's code, not at the call site":
        "interp",
    "Hot base R closures get hash-guarded native prologues":
        "builtins",
    "Tier 0 records feedback only for bodies halfway to a threshold":
        "jit-ssa",
    "Bodies that could run nothing natively never record":
        "jit-ssa",
    "Builtin fast paths take names as CHARSXPs and evaluate nothing twice":
        "builtins",
    "One-argument builtins and small base closures skip dispatch when their arguments are values":
        "builtins",
    "The native execution model: one design for frames, calls, unboxing and install-time code":
        "jit-ssa",
    "Single-core speed first; per-process costs count once per worker":
        "interp",
    "Builtin call machinery and subassignment toward 10x":
        "builtins",
    "Computed subscripts stay in bytecode":
        "jit-ssa",
    "Package code that dominates a workload runs natively: rlang's trace_back":
        "builtins",
    "Environment variable calls read `environ` once":
        "builtins",
    "Conditions: message(), warning(), signalCondition() and getOption() run natively when nothing could observe the difference":
        "builtins",
    "One-pass attribute lookup and the blocked matrix product":
        "builtins",
    "Inline small-vector allocation":
        "heap-gc",
    "unlink() follows GNU R's R_unlink, with directory descriptors":
        "builtins",
    "The package corpus runs offline":
        "r-compat",
    "Parse data as dense columns":
        "builtins",
    "The oracle suite names the GNU R build it was recorded with":
        "r-compat",
    "Stats distribution closures enter their routine directly":
        "builtins",
    "package:ry attaches no bare name another package exports":
        "frames-table",
    "Hot C API entry points take a direct success path":
        "libr-rcmd",
    "Tier-0 call entry: raw pool reads, an out-of-line tier schedule, and a cleared register window":
        "jit-ssa",
    "Tier-0 global guards read two bits and the base binding":
        "jit-ssa",
    "Workspace-wide feature unification":
        "tooling-bench-ci",
    "Evaluated arguments are rooted, and `gc()` runs queued finalizers first":
        "heap-gc",
    "Base `.Internal` wrappers inlined; string fast paths not kept":
        "builtins",
    "The oracle gives every engine the reference R's library paths":
        "r-compat",
    "A crash is never a known failure":
        "r-compat",
    "The serialize reader checks what a stream claims before it stores it":
        "builtins",
    "sprintf formats numbers with the native kernels":
        "builtins",
    "A typed call that runs in the interpreter always says why":
        "typed-stdlib",
    "Top-level loops compile per evaluation, without a call frame or a tier":
        "jit-ssa",
    "Hash tables for `unique()` and `match()` stop growing with the elements at 256 KB":
        "builtins",
    "Compiled code calls an active binding at every read":
        "jit-ssa",
    "Positional builtin calls read their arguments from the registers":
        "builtins",
    "A small typed function is lowered into its caller":
        "typed-stdlib",
    "A typed free calls a register-preserving entry":
        "typed-stdlib",
    "Fused list kernels are compiled per CPU feature level and clear no buffers":
        "typed-stdlib",
    "Short strings are built and hashed as two words":
        "typed-stdlib",
    "One thread-local holds every typed allocation pool":
        "typed-stdlib",
    "An `I64` remainder or quotient by a positive constant needs no checks":
        "typed-stdlib",
    "A warning reaches `suppressWarnings` and `tryCatch` without running R":
        "builtins",
    "A compiled return writes locals back only when something can still read the frame":
        "jit-ssa",
    "A native recursive function's depth budget shrinks with the interpreter depth":
        "jit-ssa",
    "The hot suite replays the package entries the code bases spend their time in":
        "tooling-bench-ci",
    "The profile summary hashes private package names with a secret key per project":
        "tooling-bench-ci",
    "`validObject` runs validity methods without its slot loop, and `new()` copies the prototype shallowly":
        "builtins",
    "A watchdog thread raises the poll flag at a time limit's deadline, and the interrupt check raises the error":
        "interp",
    "Beta semantics: The R variable model is mutable slots with union joins":
        "syntax-semantics",
    "Beta semantics: The boundary for R's object systems is dispatch, not Hindley-Milner":
        "syntax-semantics",
    "Beta semantics: The expanded parameter directive is `@param name {TYPE}`":
        "syntax-semantics",
    "There is no third constraint kind, so two flexible comparison operands stay unconstrained":
        "syntax-semantics",
    "Package tests through the oracle":
        "r-compat",
    "The default packages' utils, tools, grDevices and graphics routines":
        "builtins",
    "Standard library updates reuse a uniquely held value":
        "typed-stdlib",
    "A call's result drops unconstrained type-parameter members of a union":
        "syntax-semantics",
    "Comparisons coerce across atomic types":
        "syntax-semantics",
    "A test-guarded write to a formal normalizes it; inferred types admit the default":
        "syntax-semantics",
    "An open `T | NULL` parameter binds `T` to the argument's non-`NULL` part":
        "syntax-semantics",
    "Nested replacement rewrites the spine":
        "syntax-semantics",
    "Cold-check constant costs: one green walk per file, immortal interning, high durability":
        "syntax-semantics",
    "Positioning":
        "docs-process",
    "Read-only element pointers escape too":
        "heap-gc",
    "data.table's C runs at or below GNU R's time: barrier, pacing and string kernels":
        "heap-gc",
    "Every host keeps every tree":
        "syntax-semantics",
    "Library leaves resolve at compile time":
        "typed-stdlib",
    "A key of the wrong type is an error, not a missing key":
        "typed-stdlib",
    "The runtime emulates the GNU R version it loads base R from":
        "libr-rcmd",
    "The overload probe's unchecked-argument witness is a nominal no candidate declares":
        "syntax-semantics",
    "Base R kernels: radix sorting, gather subsetting, x87 accumulation and shortest-digit formatting":
        "builtins",
    "The language server reads the check record":
        "ide-server",
    "`$` fields resolve through the values that built the receiver":
        "syntax-semantics",
    "`ry test --jobs` runs test files in testthat's workers, loaded by ry's loader":
        "libr-rcmd",
    "References are checked against an independent resolver, and a replacement copy is its source":
        "syntax-semantics",
    "`if` in NAMESPACE is evaluated by the host, then resolved in text":
        "syntax-semantics",
    "A literal `source()` path is followed for resolution":
        "syntax-semantics",
    "A splice reparse refuses a prefix with an unclosed delimiter":
        "syntax-semantics",
    "Refactorings refuse rather than guess, and a seeded property check holds them to it":
        "ide-server",
    "The syntax tree is ry's own flat preorder arena":
        "syntax-semantics",
    "Completion pushes down names whose declared type cannot fit, and never promotes":
        "ide-server",
    "A cold check holds one parse tree per worker":
        "syntax-semantics",
    "Every host resolves names through one path, and trees live in the host's store":
        "syntax-semantics",
    "The analysis core's per-item and per-file tables after the type store":
        "syntax-semantics",
    "An installed typed package holds elaborated sources with canonical annotations":
        "typed-stdlib",
    "The typed core lowers R syntax at the first call and runs on 16-byte strings and view lists":
        "typed-stdlib",
    "Installed packages load through native fast paths, not an image":
        "libr-rcmd",
    "Code images: compiled code beside the lazy-load database":
        "heap-gc",
    "Numeric routines are exact ports of GNU R 4.6.2, and LINPACK lives in the interpreter crate":
        "builtins",
    "The untyped performance roadmap is ordered by measured gap, and the baseline tier is Sparkplug-style":
        "jit-ssa",
    "Builtins with positional direct entries, one implementation per semantic":
        "builtins",
    "Minor collections touch only what changed":
        "heap-gc",
    "Start-up and code images are keyed by build inside a base R tree":
        "heap-gc",
    "Typed `List.sort_with` is a merge sort with its comparator inlined, and strings order by the interpreter's collation":
        "typed-stdlib",
    "The typed suite scores typed ry against Rust":
        "typed-stdlib",
    "Shapes are confirmed after lowering, and machine code is generated once":
        "typed-stdlib",
    "Tier 1 leaves compiled code through frame states":
        "jit-ssa",
    "The interpreter is an engine crate between a numeric crate and four leaf crates":
        "interp",
    "`%%` and `%/%` on doubles port GNU R's kernels, with a 64-bit path for whole numbers":
        "builtins",
}

# Keyword weights per area. A heading hit counts HEADING_WEIGHT times a body hit.
KEYWORDS = {
    "syntax-semantics": [
        "parser", "syntax tree", "green tree", "lossless", "salsa", "hir", "lowering", "resolve",
        "resolution", "type checker", "checker", "inference", "unif", "hindley", "overload",
        "union", "narrow", "annotation", "stub", "strict mode", "fixpoint", "scheme",
        "crates/syntax", "crates/semantics", "interface", "firewall", "namespace", "masked",
        "type error", "diagnostic", "liveness", "generaliz",
    ],
    "ide-server": [
        "language server", "lsp", "editor", "hover", "completion", "goto", "rename",
        "inlay", "call hierarchy", "workspace symbol", "formatter", "format", "lint", "refactor",
        "crates/ide", "crates/format", "did_change", "zed", "vs code", "vscode", "code action",
        "signature help", "miette", "check record", "debug adapter", "console", "watch",
    ],
    "interp": [
        "interpreter", "crates/interp", "promise", "closure call", "environment",
        "lookup", "dispatch", "eval", "frame", "condition", "s3", "argument", "call setup",
        "binding", "slot frame",
    ],
    "builtins": [
        "builtin", "kernel", "base r", "regex", "regular expression", "connection", "sprintf",
        "radix", "crates/numeric", "rmath", "rng", "distribution", "lapack", "blas", "linpack",
        "pcre", "tre", "methods package", "s4", "utils", "grdevices", "graphics", "stats",
        "serialize", "natively", "native prologue", "withr", "rlang",
    ],
    "r-compat": [
        "gnu r's", "oracle", "corpus", "testthat", "package test", "artifact", "compatib",
        "matches gnu r", "as gnu r", "gnu r does", "gnu r prints", "reference r", "known failure",
    ],
    "jit-ssa": [
        "jit", "tier 0", "tier-0", "tier 1", "tier-1", "cranelift", "ssa", "deopt",
        "compiled code", "compile", "inline cache", "type feedback", "crates/jit", "crates/ssa",
        "superinstruction", "quicken", "guard", "machine code", "native function", "baseline tier",
        "osr", "frame state", "inlin",
    ],
    "heap-gc": [
        "heap", "collector", "garbage", "gc", "minor collection", "young", "old generation",
        "allocation", "allocator", "mimalloc", "arena", "crates/heap", "image", "tenure",
        "marking", "finalizer", "reference count", "huge page", "rss", "pool", "card mark",
        "sexp", "write barrier", "intern",
    ],
    "libr-rcmd": [
        "libr", "c api", "r cmd", "crates/libr", "crates/rcmd", "install", "package",
        "loader", "ry load", "load_all", "reload", ".call", "native routine", "usedynlib",
        "header", "abi", "makeconf", "r home", "r_home", "dll", "macos", "windows", "lazy-load",
        "namespace load", "base r tree", "recommended packages",
    ],
    "typed-stdlib": [
        "typed", "standard library", "std", "record", "tuple", "tag", "list.", "str.", "dict",
        "result", "crates/typed", "aot", "ahead of time", "i64", "native value", "box",
        "roc", "match", "typed boundary",
    ],
    "frames-table": [
        "dataframe", "data frame", "table engine", "polars", "group-by", "group by", "join",
        "csv", "sniff", "data.table", "crates/frames", "column", "factor", "kernel", "sort",
        "pivot", "query",
    ],
    "tooling-bench-ci": [
        "benchmark", "bench", "release", "ci", "nix", "flake", "build profile", "lto", "pgo",
        "profile-guided", "cargo", "sccache", "cachix", "glibc", "zig", "fuzz", "harness",
        "seed", "battery", "scenario", "simulat", "worktree", "dev shell", "toolchain",
        "binary", "platform", "crash", "signal", "just gate",
    ],
    "docs-process": [
        "docs", "documentation", "readme", "landing", "licens", "positioning", "agent",
        "memory", "decisions.md", "backlog", "information architecture", "user directive",
        "roadmap", "sidebar",
    ],
}
HEADING_WEIGHT = 4
# A classification whose best score is below this many times the runner-up is reported as low
# confidence, so a reviewer can pin it with an override.
CONFIDENCE_RATIO = 1.3

INDEX_PREAMBLE = """\
Settled architecture decisions and the reasons behind them, one file per area. Every entry carries a `**Status:**` line under its heading:

- **built**: the code does what the entry says.
- **planned**: the design is settled and not fully implemented. The line says what exists so far.
- **superseded**: another entry replaced this one. The line names it and says what the text is kept for. A superseded entry with nothing left to teach is deleted.

A new entry gets its status line when it is written, and the line changes in the same change that builds or replaces the design. A new entry is an H2 section appended to the area file it belongs to; then run `python3 tools/memory.py index` to regenerate this file, which `python3 tools/memory.py check` (part of `just gate`) keeps honest.
"""


def main(argv):
    if len(argv) != 2 or argv[1] not in ("split", "index", "check"):
        print(__doc__.strip(), file=sys.stderr)
        return 2
    command = argv[1]
    if command == "split":
        return split()
    if command == "index":
        write_index()
        return 0
    return check()


Entry = collections.namedtuple("Entry", "heading body")


def split():
    if not os.path.exists(LEGACY):
        print(f"nothing to split: {rel(LEGACY)} does not exist")
        return 0
    legacy = parse_legacy(read(LEGACY))
    areas = {name: read_area(name) for name in AREA_NAMES}
    before = collections.Counter(entry.body for entries in areas.values() for entry in entries)
    location = {}
    for name, entries in areas.items():
        for position, entry in enumerate(entries):
            location[entry.heading] = (name, position)

    headings = collections.Counter(entry.heading for entry in legacy)
    duplicates = [heading for heading, count in headings.items() if count > 1]
    if duplicates:
        fail(f"{rel(LEGACY)} repeats these headings; rename one of each pair first:\n  "
             + "\n  ".join(duplicates))

    added = collections.Counter()
    updated = collections.Counter()
    replaced = collections.Counter()
    incoming = collections.Counter()
    low_confidence = []
    for entry in legacy:
        if entry.heading in location:
            name, position = location[entry.heading]
            old = areas[name][position]
            if old.body != entry.body:
                replaced[old.body] += 1
                incoming[entry.body] += 1
                areas[name][position] = entry
                updated[name] += 1
            continue
        incoming[entry.body] += 1
        name, confident, scores = classify(entry)
        if not confident:
            low_confidence.append((entry.heading, name, scores))
        location[entry.heading] = (name, len(areas[name]))
        areas[name].append(entry)
        added[name] += 1

    # The multiset of bodies after the split is exactly the bodies the area files held, minus the
    # ones a newer legacy version replaced, plus the legacy entries added or updated.
    after = collections.Counter(entry.body for entries in areas.values() for entry in entries)
    legacy_bodies = collections.Counter(entry.body for entry in legacy)
    if after != before - replaced + incoming or legacy_bodies - after:
        fail("the split would lose or alter an entry body; nothing was written")

    os.makedirs(AREA_DIR, exist_ok=True)
    for name in AREA_NAMES:
        if added[name] or updated[name] or not os.path.exists(area_path(name)):
            write(area_path(name), render_area(name, areas[name]))
    if reparse_bodies() != after:
        fail("the area files do not read back as the entries just written")
    os.remove(LEGACY)
    write_index()

    print(f"{'area':<18} {'entries':>7} {'bytes':>9} {'added':>6} {'updated':>8}")
    for name in AREA_NAMES:
        size = os.path.getsize(area_path(name))
        print(f"{name:<18} {len(areas[name]):>7} {size:>9} {added[name]:>6} {updated[name]:>8}")
    if low_confidence:
        print(f"\nlow-confidence classifications ({len(low_confidence)}); pin them in OVERRIDES:")
        for heading, name, scores in low_confidence:
            ranked = ", ".join(f"{area} {score}" for area, score in scores[:3])
            print(f"  {name:<18} {heading[:90]}  [{ranked}]")
    print(f"\nremoved {rel(LEGACY)}; stage with `git add -A {rel(MEMORY)}`")
    return 0


def parse_legacy(text):
    """Entries of the single-file log: every H2, and every H1 record with body text of its own."""
    entries = []
    record = None
    for level, heading, body in sections(text):
        if level == 1:
            record = heading
            if heading == "Decisions" or not body.strip():
                continue
            title = heading[len(RECORD_PREFIX):] if heading.startswith(RECORD_PREFIX) else heading
            entries.append(Entry(title[:1].upper() + title[1:], body))
        else:
            label = FOLD_RECORD.get(record)
            entries.append(Entry(f"{label}: {heading}" if label else heading, body))
    # Only the file's own title section ("# Decisions") is dropped; every other line of text must
    # land in an entry body.
    text_lines = sum(1 for _, _, body in sections(text) for line in body.splitlines() if line.strip())
    kept = sum(1 for entry in entries for line in entry.body.splitlines() if line.strip())
    preamble = sum(1 for level, heading, body in sections(text) if level == 1 and heading == "Decisions"
                   for line in body.splitlines() if line.strip())
    if text_lines != kept + preamble:
        fail(f"{rel(LEGACY)} has {text_lines - kept - preamble} lines outside any entry")
    return entries


def read_area(name):
    path = area_path(name)
    if not os.path.exists(path):
        return []
    entries = []
    for level, heading, body in sections(read(path)):
        if level == 1:
            if body.strip() and body.strip() != area_blurb(name):
                fail(f"{rel(path)} has text under its title; move it into an entry")
            continue
        entries.append(Entry(heading, body))
    return entries


def sections(text):
    """(level, heading, body) for each H1 and H2 outside code fences. A body runs from the line
    after its heading to the next H1 or H2, with trailing blank lines normalized to none."""
    result = []
    current = None
    fenced = False
    for line in text.splitlines(keepends=True):
        stripped = line.rstrip("\n")
        if stripped.lstrip().startswith(("```", "~~~")):
            fenced = not fenced
        heading = None
        if not fenced:
            if stripped.startswith("# "):
                heading = (1, stripped[2:].strip())
            elif stripped.startswith("## "):
                heading = (2, stripped[3:].strip())
        if heading:
            if current:
                result.append(current)
            current = [heading[0], heading[1], ""]
        elif current:
            current[2] += line
    if current:
        result.append(current)
    return [(level, heading, normalize(body)) for level, heading, body in result]


def normalize(body):
    return body.rstrip("\n") + "\n"


def classify(entry):
    if entry.heading in OVERRIDES:
        return OVERRIDES[entry.heading], True, []
    heading = entry.heading.lower()
    body = entry.body.lower()
    scores = []
    for name in AREA_NAMES:
        score = 0
        for keyword in KEYWORDS[name]:
            pattern = keyword_pattern(keyword)
            score += HEADING_WEIGHT * len(pattern.findall(heading)) + len(pattern.findall(body))
        scores.append((name, score))
    scores.sort(key=lambda item: (-item[1], AREA_NAMES.index(item[0])))
    best, runner_up = scores[0], scores[1]
    confident = best[1] > 0 and best[1] >= CONFIDENCE_RATIO * max(runner_up[1], 1)
    return best[0], confident, scores


_PATTERNS = {}


def keyword_pattern(keyword):
    if keyword not in _PATTERNS:
        # Keywords match at a word start, so `gc` does not hit `logic` and `ci` does not hit `decision`.
        tail = r"\b" if len(keyword) <= 3 else ""
        _PATTERNS[keyword] = re.compile(r"(?<![\w])" + re.escape(keyword) + tail)
    return _PATTERNS[keyword]


def render_area(name, entries):
    _, title, _ = area_info(name)
    parts = [f"# Decisions: {title}\n\n{area_blurb(name)}\n"]
    for entry in entries:
        parts.append(f"\n## {entry.heading}\n{entry.body}")
    return "".join(parts)


def reparse_bodies():
    return collections.Counter(entry.body for name in AREA_NAMES for entry in read_area(name))


def render_index():
    lines = ["# Decisions index\n", "\n", INDEX_PREAMBLE]
    for name, title, blurb in AREAS:
        entries = read_area(name)
        lines.append(f"\n## {title}\n\n[`{name}.md`]({name}.md): {blurb}\n\n")
        slugs = collections.Counter()
        for entry in entries:
            slug = github_slug(entry.heading)
            anchor = slug if not slugs[slug] else f"{slug}-{slugs[slug]}"
            slugs[slug] += 1
            status = status_of(entry.body)
            suffix = f" ({status})" if status else ""
            lines.append(f"- [{escape_link(entry.heading)}]({name}.md#{anchor}){suffix}\n")
    return "".join(lines)


def write_index():
    write(INDEX, render_index())


def status_of(body):
    for line in body.splitlines()[:6]:
        if line.startswith("**Status:**"):
            text = line[len("**Status:**"):].strip()
            superseded = re.match(r'superseded by "([^"]+)"', text)
            if superseded:
                return f'superseded by "{superseded.group(1)}"'
            word = re.match(r"[a-z]+", text)
            if not word:
                return text[:60]
            rest = text[word.end():]
            return word.group(0) + (", with exceptions" if rest.startswith(", except") else "")
    return None


def github_slug(heading):
    text = heading.lower()
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


def escape_link(text):
    return text.replace("[", "\\[").replace("]", "\\]")


def check():
    problems = []
    if os.path.exists(LEGACY):
        problems.append(
            f"{rel(LEGACY)} exists. Decisions live in {rel(AREA_DIR)}/<area>.md: move each entry "
            f"to the area it belongs to (or run `python3 tools/memory.py split`), then run "
            f"`python3 tools/memory.py index`.")
    known = {f"{name}.md" for name in AREA_NAMES} | {"index.md"}
    if os.path.isdir(AREA_DIR):
        for file in sorted(os.listdir(AREA_DIR)):
            if file not in known:
                problems.append(
                    f"{rel(os.path.join(AREA_DIR, file))} is not a known area. Move its entries "
                    f"into one of {', '.join(AREA_NAMES)}, or add the area to tools/memory.py.")
        stale = not os.path.exists(INDEX) or read(INDEX) != render_index()
        if stale:
            problems.append(f"{rel(INDEX)} is stale. Run `python3 tools/memory.py index`.")
    for path, budget in BUDGETS.items():
        full = os.path.join(ROOT, path)
        if not os.path.exists(full):
            continue
        size = os.path.getsize(full)
        if size > budget:
            problems.append(
                f"{path} is {size} bytes, over its budget of {budget}. Prune resolved or stale "
                f"material (or move it to the right horizon) rather than raising the budget in "
                f"tools/memory.py.")
    for path, limit in LINE_LIMITS.items():
        full = os.path.join(ROOT, path)
        if not os.path.exists(full):
            continue
        for number, line in enumerate(read(full).splitlines(), 1):
            if len(line.encode()) > limit:
                problems.append(
                    f"{path}:{number} is {len(line.encode())} bytes, over the line limit of "
                    f"{limit}. Shorten it to the essential outcome.")
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    return 1 if problems else 0


def area_info(name):
    for area in AREAS:
        if area[0] == name:
            return area
    raise KeyError(name)


def area_blurb(name):
    return area_info(name)[2]


def area_path(name):
    return os.path.join(AREA_DIR, f"{name}.md")


def rel(path):
    return os.path.relpath(path, ROOT)


def read(path):
    with open(path, encoding="utf-8", newline="") as file:
        return file.read()


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="") as file:
        file.write(text)


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
