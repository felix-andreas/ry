#!/usr/bin/env python3
"""What one R-level mechanism costs on ry, in instructions: a closure call, an S3 dispatch, a
`tryCatch`, a builtin call.

Each operation below is an expression that a compiled loop evaluates. The tool runs the loop under
callgrind for two iteration counts and reports the difference per iteration, less the cost of the
empty loop, so start-up, compilation and warm-up cancel and the number repeats exactly. The tier-1
JIT is off by default, because package code runs in tier 0 almost all of the time; `--jit` leaves
it on.

Usage: tools/bench/tooling/mechanisms.py [--ry PATH] [--filter TEXT]... [--jit] [--save FILE]
                                         [--baseline FILE] [--profile NAME] [--top N] [--gnu-r]

`--gnu-r` counts the same loops on GNU R (the `R` on the PATH) and adds its count and ry's count
over it, so the operations where ry runs more instructions than GNU R stand out.

`--save FILE` keeps the counts and `--baseline FILE` compares against an earlier `--save`.
`--profile NAME` prints the functions that hold the most of one operation's instructions, which
needs a build with symbols (`cargo build --profile profiling`, or `--profile iterate`).
"""
import argparse, concurrent.futures, json, os, pathlib, re, shutil, subprocess, sys, tempfile

root = pathlib.Path(__file__).resolve().parents[3]

# Both counts are past every warm-up threshold, so each iteration between them costs the same.
SHORT, LONG = 20_000, 40_000

# Bindings every operation may use: `i` is the loop variable, an integer.
PRELUDE = """
object <- structure(list(a = 1L, b = "x"), class = "widget")
child <- structure(list(a = 1L), class = c("gadget", "widget"))
plain <- list(a = 1L, b = "x")
numbers <- c(1.5, 2.5, 3.5, 4.5)
named <- c(a = 1, b = 2, c = 3)
words <- c("alpha", "beta", "gamma")
holder <- new.env()
holder$value <- 1L
holder$method <- function(x) x + holder$value
k0 <- function() 1L + 1L
kconstant <- function() NULL
kidentity <- function(x) x
k1 <- function(x) x + 1L
kif <- function(x) if (is.null(x)) 0L else x + 1L
k2 <- function(x, y = 2L) x + y
k3 <- function(a, b, c) a + b + c
klocal <- function(x) { y <- x + 1L; z <- y + 1L; z }
kdots <- function(...) k1(...)
kdots_list <- function(...) length(list(...))
kmissing <- function(x, y) if (missing(y)) x + 1L else y
kclosure <- function(x) { inner <- function() x + 1L; inner() }
kmaker <- function(x) function() x
kexit <- function(x) { on.exit(NULL); x + 1L }
kmatch <- function(kind = c("first", "second")) { kind <- match.arg(kind); kind }
kstop <- function(x) { stopifnot(is.numeric(x)); x }
kparent <- function() parent.frame()
ksyscall <- function(x) sys.call()
kinvisible <- function(x) invisible(x)
kreturn <- function(x) { if (x > 0L) return(x); 0L }
kpeek <- function() { parent.frame(); 1L }
kwatched <- function(x) { kpeek(); x }
kassign <- function(x) { assign("y", x); x }
kassign_call <- function(x) { assign("y", x); k1(x) }
kapply <- function(f, x) f(x)
kbuiltin <- function(x) { unclass(x) }
describe <- function(x, ...) UseMethod("describe")
describe.default <- function(x, ...) if (is.null(x)) 0L else 1L
describe.widget <- function(x, ...) if (is.null(x)) 0L else 2L
describe.gadget <- function(x, ...) NextMethod()
length.widget <- function(x) if (is.null(x)) 0L else 7L
"[.widget" <- function(x, i) if (is.null(x)) 0L else i
"$.gadget" <- function(x, name) if (is.null(x)) 0L else name
format.widget <- function(x, ...) if (is.null(x)) "" else "widget"
quoted <- quote(i + 1L)
grid <- matrix(0, 4L, 4L)
wide <- matrix(0, 10L, 10L)
kstore <- function(v) { v[2L] <- 9; v }
kgrid <- function(m) { m[1L, 1L] <- 1; m[2L, 2L] <- 1; m }
kappend <- function() { x <- numeric(0); x[[1L]] <- 1; x[[2L]] <- 2; x[[3L]] <- 3; x }
kfield <- function(l) { l$z <- 1; l }
kcount <- local({ count <- 0L; function() { count <<- count + 1L; count } })
kkeep <- local({ kept <- numeric(4L); function(v) { kept[2L] <<- v; 0L } })
kkeep_list <- local({ kept <- list(1, 2); function(v) { kept[[2L]] <<- v; 0L } })
kkeep_field <- local({ kept <- list(a = 1, b = 2); function(v) { kept$b <<- v; 0L } })
ten <- as.list(as.numeric(1:10))
named_list <- list(a = 1, b = 2, c = 3)
nested <- list(inner = list(low = 1, high = 2), size = 3)
frame <- data.frame(col1 = c("a", "b", "c"), col3 = c(1, 2, 3), stringsAsFactors = FALSE)
kanonymous <- function(x) (function(y) y + 1L)(x)
kdefaults <- function(x) (function(a, b = 2L, d = NULL) if (missing(d)) a + b else a)(x)
kblock <- function(x) local({ a <- x; a + 1L })
kpasses <- function(x) kapply(function(z) z + x, x)
klapply <- function(x) lapply(1:3, function(v) v + x)
kexit_inner <- function(x) (function() { on.exit(NULL); x })()
kcalling <- function(x) withCallingHandlers(k1(x), warning = function(w) NULL)
kdepth <- function(x) { deeper <- function(d) if (d == 0L) sys.calls() else deeper(d - 1L); deeper(3L) }
klist <- function(...) list(...)
knested <- function(l, v) { l$inner$low <- v; l }
kswitch <- function(kind) switch(kind, a = 1L, b = 2L, c = 3L, 4L)
kchain <- function(kind) if (kind == "a") 1L else if (kind == "b") 2L else if (kind == "c") 3L else 4L
kcopy <- function(l) { out <- list(); for (name in c("a", "b")) out[[name]] <- l[[name]]; out }
kobject <- (function() {
  values <- list(1L)
  list(get = function(key) values[[key]])
})()
kobject_wide <- (function() {
  a1 <- 1; a2 <- 1; a3 <- 1; a4 <- 1; a5 <- 1; a6 <- 1; a7 <- 1; a8 <- 1; a9 <- 1; a10 <- 1
  b1 <- 1; b2 <- 1; b3 <- 1; b4 <- 1; b5 <- 1; b6 <- 1; b7 <- 1; b8 <- 1; b9 <- 1; b10 <- 1
  values <- list(1L)
  get <- function(key) values[[key]]
  checked <- function(key) if (is.null(key)) 0L else get(key)
  list(get = get, checked = checked)
})()
"""

# Bindings of the `s4:` operations, which only their scripts define, so the other operations'
# counts do not depend on what the methods package has registered.
S4_PRELUDE = """
setClass("Base", representation("VIRTUAL", id = "character"))
setClass("Item", contains = "Base", representation(value = "numeric", meta = "list"),
  validity = function(object) if (length(object@id) != 1L) "id must have length 1" else TRUE
)
setClass("SubItem", contains = "Item", representation(factor = "numeric"))
setGeneric("id", function(object) standardGeneric("id"))
setMethod("id", "Base", function(object) object@id)
setGeneric("getValue", function(object, ...) standardGeneric("getValue"))
setMethod("getValue", "Item", function(object, key = NA_character_, scale = 1, ...) {
  if (!is.na(key)) return(object@meta[[key]])
  object@value * scale
})
setMethod("getValue", "SubItem", function(object, key = NA_character_, scale = 1, ...) {
  callNextMethod() * object@factor
})
setGeneric("combine", function(left, right) standardGeneric("combine"))
setMethod("combine", signature("Item", "Item"), function(left, right) left@value + right@value)
setMethod("combine", signature("Item", "missing"), function(left, right) left@value)
setGeneric("value<-", function(object, value) standardGeneric("value<-"))
setReplaceMethod("value", "Item", function(object, value) {
  object@value <- value
  object
})
setMethod("length", "Item", function(x) 3L)
item <- new("Item", id = "a", value = 1, meta = list(k = 2))
sub <- new("SubItem", id = "b", value = 1, meta = list(k = 2), factor = 3)
"""

# (name, expression). The loop assigns the expression's value, so an operation's count includes
# one assignment to a local, as the empty loop's does.
OPERATIONS = [
    ("call: no arguments", "k0()"),
    ("call: body is a constant", "kconstant()"),
    ("call: body is its argument", "kidentity(i)"),
    ("call: one argument, body is one builtin call", "k1(i)"),
    ("call: one argument, body branches", "kif(i)"),
    ("call: default argument used", "k2(i)"),
    ("call: arguments by name", "k2(x = i, y = 3L)"),
    ("call: three arguments", "k3(i, 1L, 2L)"),
    ("call: body with locals", "klocal(i)"),
    ("call: forwards `...`", "kdots(i)"),
    ("call: collects `...` in a list", "kdots_list(i, 1L)"),
    ("call: tests `missing()`", "kmissing(i)"),
    ("call: makes and calls a closure", "kclosure(i)"),
    ("call: returns a closure", "kmaker(i)"),
    ("call: calls an anonymous function", "kanonymous(i)"),
    ("call: calls an anonymous function with defaults", "kdefaults(i)"),
    ("call: `local()` block", "kblock(i)"),
    ("call: passes a closure on", "kpasses(i)"),
    ("call: `lapply` over 3 with a closure", "klapply(i)"),
    ("call: `on.exit`", "kexit(i)"),
    ("call: `on.exit` in an anonymous function", "kexit_inner(i)"),
    ("call: `sys.calls()` 3 frames deep", "kdepth(i)"),
    ("call: `list(...)`", 'klist("a", "b", i)'),
    ("call: `switch` on a string", 'kswitch("c")'),
    ("call: `if` chain on a string", 'kchain("c")'),
    ("call: `match.arg`", "kmatch()"),
    ("call: `stopifnot`", "kstop(i)"),
    ("call: `parent.frame()`", "kparent()"),
    ("call: `sys.call()`", "ksyscall(i)"),
    ("call: `invisible`", "kinvisible(i)"),
    ("call: early `return`", "kreturn(i)"),
    ("call: its callee reads `parent.frame()`", "kwatched(i)"),
    ("call: `assign` to a local", "kassign(i)"),
    ("call: `assign` to a local, then a call", "kassign_call(i)"),
    ("call: a function expression as an argument", "kapply(function(z) z + 1L, i)"),
    ("call: body is one builtin call", "kbuiltin(i)"),
    ("call: function held in an environment", "holder$method(i)"),
    ("call: `do.call`", "do.call(k1, list(i))"),
    ("call: `eval` of a quoted call", "eval(quoted)"),
    ("call: anonymous function in place", "(function(x) x + 1L)(i)"),
    ("call: its value replaces its argument", "kidentity(result)"),
    ("call: base closure with a native entry", "isTRUE(result)"),
    ("call: native entry declines a call argument", "isTRUE(kidentity(i))"),
    ("call: method of a closure object", "kobject$get(1L)"),
    ("call: method of a closure object with 23 variables", "kobject_wide$get(1L)"),
    ("call: method of a closure object calls another", "kobject_wide$checked(1L)"),
    ("apply: `lapply` over 4 elements", "lapply(numbers, k1)"),
    ("apply: `vapply` over 4 elements", "vapply(numbers, k1, 0)"),
    ("apply: `sapply` over 4 elements", "sapply(numbers, k1)"),
    ("apply: `Map` over 4 elements", "Map(k2, numbers, numbers)"),
    ("dispatch: `UseMethod`, class method", "describe(object)"),
    ("dispatch: `UseMethod`, default method", "describe(i)"),
    ("dispatch: `UseMethod` then `NextMethod`", "describe(child)"),
    ("dispatch: builtin to a method (`length`)", "length(object)"),
    ("dispatch: `[` to a method", "object[1L]"),
    ("dispatch: `$` to a method", "child$a"),
    ("dispatch: `format` generic", "format(object)"),
    ("dispatch: `inherits`", 'inherits(object, "widget")'),
    ("s4: generic with one argument", "id(item)"),
    ("s4: generic, method inherited from a superclass", "id(sub)"),
    ("s4: generic on two arguments", "combine(item, sub)"),
    ("s4: generic, a signature argument missing", "combine(item)"),
    ("s4: method with more arguments than its generic", "getValue(item, scale = 2)"),
    ("s4: `callNextMethod()`", "getValue(sub)"),
    ("s4: replacement generic", "{ value(item) <- i; 0L }"),
    ("s4: builtin to a method (`length`)", "length(item)"),
    ("s4: slot read", "item@value"),
    ("s4: slot write", "{ item@value <- 2; 0L }"),
    ("s4: `new`", 'new("Item", id = "a", value = 1)'),
    ("s4: `is`", 'is(item, "Base")'),
    ("s4: `isVirtualClass`", 'isVirtualClass("Base")'),
    ("s4: `validObject`", "validObject(item)"),
    ("condition: `tryCatch`, no condition", "tryCatch(i + 1L, error = function(e) 0L)"),
    ("condition: `tryCatch` with `finally`", "tryCatch(i + 1L, finally = NULL)"),
    ("condition: `tryCatch` catches `stop`", 'tryCatch(stop("no"), error = function(e) 0L)'),
    ("condition: `try`, no condition", "try(i + 1L, silent = TRUE)"),
    ("condition: `withCallingHandlers`, no condition", "withCallingHandlers(i + 1L, warning = function(w) 0L)"),
    ("condition: `withCallingHandlers` in a function, no condition", "kcalling(i)"),
    ("condition: `suppressWarnings`, no condition", "suppressWarnings(i + 1L)"),
    ("condition: `warning` muffled", 'withCallingHandlers(warning("w"), warning = function(w) invokeRestart("muffleWarning"))'),
    ("condition: `signalCondition`, no handler", 'signalCondition(simpleCondition("m"))'),
    ("builtin: `length`", "length(plain)"),
    ("builtin: `is.null`", "is.null(plain)"),
    ("builtin: `as.integer` of an integer", "as.integer(i)"),
    ("builtin: `[[` by position", "plain[[1L]]"),
    ("builtin: `[[` by name", 'plain[["b"]]'),
    ("builtin: `$`", "plain$b"),
    ("builtin: `[` by position", "numbers[2L]"),
    ("builtin: `[` by name", 'named["b"]'),
    ("builtin: `names`", "names(named)"),
    ("builtin: `c` of two scalars", "c(i, 1L)"),
    ("builtin: `c` of two named scalars", "c(a = i, b = 1L)"),
    ("builtin: `list` with a name", "list(a = i)"),
    ("builtin: `list` of two values", "list(i, 1L)"),
    ("builtin: `list` of three values, two named", 'list(a = i, "b", c = 1L)'),
    ("builtin: `paste0`", 'paste0("a", i)'),
    ("builtin: `sprintf`", 'sprintf("%d", i)'),
    ("builtin: `identical`", "identical(i, 1L)"),
    ("builtin: `is.numeric && length`", "is.numeric(i) && length(i) == 1L"),
    ("builtin: `%in%`", '"beta" %in% words'),
    ("builtin: `nchar`", "nchar(words)"),
    ("builtin: `nzchar`", "nzchar(words)"),
    ("builtin: `substr`", "substr(words, 1L, 2L)"),
    ("builtin: `toupper`", "toupper(words)"),
    ("builtin: `startsWith`", 'startsWith(words, "a")'),
    ("builtin: `strtrim`", "strtrim(words, 2L)"),
    ("builtin: `grepl`, fixed", 'grepl("a", words, fixed = TRUE)'),
    ("builtin: `sub`", 'sub("a", "b", words)'),
    ("builtin: `gsub`, fixed", 'gsub("a", "b", words, fixed = TRUE)'),
    ("builtin: `regexpr`", 'regexpr("a", words)'),
    ("builtin: `bitwAnd`", "bitwAnd(i, 3L)"),
    ("builtin: `sum` of 4 numbers", "sum(numbers)"),
    ("builtin: `seq_len`", "seq_len(4L)"),
    ("builtin: `attr`", 'attr(object, "class")'),
    ("builtin: `class`", "class(object)"),
    ("builtin: `dim`", "dim(grid)"),
    ("builtin: `unclass`", "unclass(object)"),
    ("builtin: `attributes`", "attributes(object)"),
    ("builtin: `unlist` of a short list", "unlist(plain)"),
    ("builtin: `environment()`", "environment()"),
    ("builtin: `==` on two strings", 'words[2L] == "beta"'),
    ("builtin: `==` on a string vector", 'words == "beta"'),
    ("builtin: `match` among three strings", 'match("beta", words)'),
    ("builtin: `unique` of three strings", "unique(words)"),
    ("builtin: `duplicated` of three strings", "duplicated(words)"),
    ("builtin: `do.call` of `c` on 10 values", 'do.call("c", ten)'),
    ("builtin: `do.call` of `c` on 3 named values", 'do.call("c", named_list)'),
    ("builtin: `unlist` of 10 numbers", "unlist(ten)"),
    ("builtin: `unlist` of a named list", "unlist(named_list)"),
    ("builtin: `sprintf` with two values", 'sprintf("%s_%d", "name", i)'),
    ("builtin: `substr` to `nchar`", 'substr("name7|x", 1, nchar("name7|x") - 2)'),
    ("builtin: `[` on a named list by names", 'named_list[c("a", "c")]'),
    ("builtin: `[[` on a data frame", 'frame[["col3"]]'),
    ("builtin: `sum`, `mean` and `max` of a range", "sum(1:10) + mean(1:10) + max(1:10)"),
    ("assign: element of a local vector", "{ numbers[2L] <- 9; 0L }"),
    ("assign: element at a computed subscript", "{ numbers[i %% 4L + 1L] <- 9; 0L }"),
    ("assign: field of a local list", "{ plain$a <- i; 0L }"),
    ("assign: `names<-`", '{ names(numbers) <- words[c(1L, 2L, 3L, 1L)]; 0L }'),
    ("assign: `attr<-`", '{ attr(numbers, "unit") <- "m"; 0L }'),
    ("assign: `attr<-` with a computed name", '{ attr(numbers, if (i > 0L) "unit" else "u") <- "m"; 0L }'),
    ("assign: `class<-`", '{ class(plain) <- "widget"; 0L }'),
    ("assign: `oldClass<-`", '{ oldClass(plain) <- "widget"; 0L }'),
    ("assign: `dim<-`", "{ dim(numbers) <- c(2L, 2L); 0L }"),
    ("assign: `length<-`", "{ length(numbers) <- 4L; 0L }"),
    ("assign: `levels<-`", "{ levels(numbers) <- words; 0L }"),
    ("assign: variable in an environment", "{ holder$value <- i; 0L }"),
    ("assign: string element of a local vector", '{ words[2L] <- "x"; 0L }'),
    ("assign: list element by name", '{ plain[["a"]] <- i; 0L }'),
    ("assign: element by name", '{ named[["b"]] <- 9; 0L }'),
    ("assign: matrix element", "{ grid[2L, 2L] <- 9; 0L }"),
    ("assign: element of an argument, copied", "kstore(numbers)"),
    ("assign: two matrix elements of an argument, copied", "kgrid(grid)"),
    ("assign: two matrix elements of a 100-element argument, copied", "kgrid(wide)"),
    ("assign: three appends to a new vector", "kappend()"),
    ("assign: a new field of a list argument", "kfield(plain)"),
    ("assign: nested field of a list argument", "knested(nested, i)"),
    ("assign: column of a data frame", "{ frame$col4 <- i; 0L }"),
    ("assign: list fields by name in a loop", "kcopy(plain)"),
    ("assign: `<<-` to an enclosing function's variable", "kcount()"),
    ("assign: `<<-` to an element of an enclosing function's vector", "kkeep(i)"),
    ("assign: `<<-` to an element of an enclosing function's list", "kkeep_list(i)"),
    ("assign: `<<-` to a field of an enclosing function's list", "kkeep_field(i)"),
]

EMPTY = "empty loop"


def main():
    parser = argparse.ArgumentParser(description="Instructions per R-level operation on ry, under callgrind.")
    parser.add_argument("--ry", default=str(root / "target" / "release" / "ry"))
    parser.add_argument("--filter", action="append", metavar="TEXT",
                        help="only operations whose name contains TEXT; several may be given")
    parser.add_argument("--jit", action="store_true", help="leave the tier-1 JIT on")
    parser.add_argument("--save", metavar="FILE", help="keep the counts in FILE")
    parser.add_argument("--baseline", metavar="FILE", help="compare with the counts an earlier --save kept")
    parser.add_argument("--profile", metavar="NAME", help="the functions holding one operation's instructions")
    parser.add_argument("--top", type=int, default=40, help="rows of --profile")
    parser.add_argument("--gnu-r", action="store_true", help="count each operation on GNU R too")
    args = parser.parse_args()
    if shutil.which("valgrind") is None:
        sys.exit("error: this runs ry under callgrind, and valgrind is not on the PATH")
    ry = os.path.abspath(args.ry)
    if not os.path.exists(ry):
        sys.exit(f"error: no ry binary at {ry}; build one or pass --ry PATH")
    wanted = [args.profile] if args.profile else args.filter
    operations = [(EMPTY, "0L")] + [
        operation for operation in OPERATIONS
        if not wanted or any(text in operation[0] for text in wanted)
    ]
    if len(operations) == 1:
        sys.exit("error: no operation matches")
    if args.profile and (args.gnu_r or args.filter):
        sys.exit("error: --profile takes one operation by name, without --filter or --gnu-r")
    if args.profile and len(operations) != 2:
        sys.exit(f"error: --profile needs one operation, and {len(operations) - 1} match: "
                 + "; ".join(name for name, _ in operations[1:]))
    with tempfile.TemporaryDirectory(prefix="ry-mechanisms-") as work:
        work = pathlib.Path(work)
        engines = ["ry", "GNU R"] if args.gnu_r and not args.profile else ["ry"]
        runs = [(engine, index, count) for engine in engines for index in range(len(operations))
                for count in (SHORT, LONG)]

        def count(run):
            engine, index, iterations = run
            script = work / f"{index}.R"
            env = dict(os.environ, OMP_NUM_THREADS="1")
            if engine == "ry":
                dump = work / f"{index}.{iterations}.callgrind"
                command = ["valgrind", "--tool=callgrind", f"--callgrind-out-file={dump}",
                           "--smc-check=all-non-file", ry, "run", str(script), str(iterations)]
                if not args.jit:
                    env["RY_JIT"] = "off"
            else:
                dump = work / f"{index}.{iterations}.gnu.callgrind"
                command = ["R", "-d", f"valgrind --tool=callgrind --callgrind-out-file={dump}", "--vanilla",
                           "--no-echo", "-f", str(script), "--args", str(iterations)]
            done = subprocess.run(command, env=env, capture_output=True, text=True)
            total = re.search(r"Collected : (\d+)", done.stderr)
            if done.returncode != 0 or total is None:
                sys.exit(f"error: {operations[index][0]} failed on {engine}:\n{done.stderr[-2000:]}")
            return run, int(total[1])

        for index in range(len(operations)):
            (work / f"{index}.R").write_text(program(*operations[index]))
        # A build's first start writes its start-up image, and a counted run that raced that
        # write would start the slow way and shift every count, so one start comes first.
        warm = subprocess.run([ry, "run", str(work / "0.R"), "1"], capture_output=True, text=True)
        if warm.returncode != 0:
            sys.exit(f"error: ry failed to run the empty loop:\n{warm.stderr[-2000:]}")
        with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count()) as pool:
            totals = dict(pool.map(count, runs))
        per_iteration, reference = (
            [(totals[engine, index, LONG] - totals[engine, index, SHORT]) / (LONG - SHORT)
             for index in range(len(operations))] if engine in engines else []
            for engine in ("ry", "GNU R")
        )
        if args.profile:
            print(f"{operations[1][0]}: `{operations[1][1]}`, {per_iteration[1] - per_iteration[0]:,.0f} instructions\n")
            print_profile(work / f"1.{SHORT}.callgrind", work / f"1.{LONG}.callgrind", args.top)
            return
    found = {name: round(per - per_iteration[0], 1) for (name, _), per in zip(operations[1:], per_iteration[1:])}
    baseline = json.load(open(args.baseline))["operations"] if args.baseline else {}
    version = subprocess.run([ry, "--version"], capture_output=True, text=True).stdout.strip()
    print(f"{version}, tier-1 JIT {'on' if args.jit else 'off'}; the empty loop costs {per_iteration[0]:,.0f} instructions an iteration\n")
    if reference:
        print(f"GNU R's empty loop costs {reference[0]:,.0f} instructions an iteration\n")
    header = ("| operation | expression | instructions |" + (" baseline | change |" if baseline else "")
              + (" GNU R | ry over GNU R |" if reference else ""))
    print(header)
    print("| --- | --- | ---: |" + (" ---: | ---: |" if baseline else "") + (" ---: | ---: |" if reference else ""))
    for position, (name, expression) in enumerate(operations[1:], start=1):
        row = f"| {name} | `{expression}` | {found[name]:,.0f} |"
        if baseline:
            before = baseline.get(name)
            row += " | |" if before is None else f" {before:,.0f} | {change(before, found[name])} |"
        if reference:
            theirs = reference[position] - reference[0]
            row += f" {theirs:,.0f} | {found[name] / theirs:.2f} |" if theirs > 0 else f" {theirs:,.0f} | |"
        print(row)
    if args.save:
        with open(args.save, "w") as file:
            json.dump({"ry": version, "jit": args.jit, "operations": found}, file, indent=1)
            file.write("\n")


def program(name, expression):
    """The script for one operation: its loop runs inside a closure, so it runs compiled."""
    prelude = PRELUDE + (S4_PRELUDE if name.startswith("s4: ") else "")
    return (prelude + "\nrun <- function(n) {\n    result <- 0L\n    for (i in seq_len(n)) result <- "
            + expression + "\n    result\n}\ninvisible(run(as.integer(commandArgs(TRUE)[1])))\n")


def change(before, after):
    return "" if before == 0 else f"{100 * (after - before) / before:+.1f}%"


def print_profile(short, long, top):
    """The functions that ran the most instructions themselves between the two iteration counts."""
    sys.path.insert(0, str(root / "tools" / "bench"))
    import bench

    counts = [bench.parse_callgrind_functions(path.read_text(errors="replace")) for path in (short, long)]
    per = {key: (counts[1].get(key, 0) - counts[0].get(key, 0)) / (LONG - SHORT) for key in counts[1]}
    total = sum(per.values())
    print("| function | object | instructions | share |")
    print("| --- | --- | ---: | ---: |")
    for (shared_object, function), cost in sorted(per.items(), key=lambda item: -item[1])[:top]:
        print(f"| {function} | {shared_object} | {cost:,.1f} | {100 * cost / total:.1f}% |")


if __name__ == "__main__":
    main()
