#!/usr/bin/env python3
"""What one R-level mechanism costs on ry, in instructions: a closure call, an S3 dispatch, a
`tryCatch`, a builtin call.

Each operation below is an expression that a compiled loop evaluates. The tool runs the loop under
callgrind for two iteration counts and reports the difference per iteration, less the cost of the
empty loop, so start-up, compilation and warm-up cancel and the number repeats exactly. The tier-1
JIT is off by default, because package code runs in tier 0 almost all of the time; `--jit` leaves
it on.

Usage: tools/bench/tooling/mechanisms.py [--ry PATH] [--filter TEXT] [--jit] [--save FILE]
                                         [--baseline FILE] [--profile NAME] [--top N]

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
k1 <- function(x) x + 1L
kif <- function(x) if (is.null(x)) 0L else x + 1L
k2 <- function(x, y = 2L) x + y
k3 <- function(a, b, c) a + b + c
klocal <- function(x) { y <- x + 1L; z <- y + 1L; z }
kdots <- function(...) k1(...)
kdots_list <- function(...) length(list(...))
kmissing <- function(x, y) if (missing(y)) x + 1L else y
kclosure <- function(x) { inner <- function() x + 1L; inner() }
kexit <- function(x) { on.exit(NULL); x + 1L }
kmatch <- function(kind = c("first", "second")) { kind <- match.arg(kind); kind }
kstop <- function(x) { stopifnot(is.numeric(x)); x }
kparent <- function() parent.frame()
ksyscall <- function(x) sys.call()
kinvisible <- function(x) invisible(x)
kreturn <- function(x) { if (x > 0L) return(x); 0L }
describe <- function(x, ...) UseMethod("describe")
describe.default <- function(x, ...) 0L
describe.widget <- function(x, ...) 1L
describe.gadget <- function(x, ...) NextMethod()
length.widget <- function(x) 7L
"[.widget" <- function(x, i) 1L
"$.gadget" <- function(x, name) 1L
format.widget <- function(x, ...) "widget"
quoted <- quote(i + 1L)
"""

# (name, expression). The loop assigns the expression's value, so an operation's count includes
# one assignment to a local, as the empty loop's does.
OPERATIONS = [
    ("call: no arguments", "k0()"),
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
    ("call: `on.exit`", "kexit(i)"),
    ("call: `match.arg`", "kmatch()"),
    ("call: `stopifnot`", "kstop(i)"),
    ("call: `parent.frame()`", "kparent()"),
    ("call: `sys.call()`", "ksyscall(i)"),
    ("call: `invisible`", "kinvisible(i)"),
    ("call: early `return`", "kreturn(i)"),
    ("call: function held in an environment", "holder$method(i)"),
    ("call: `do.call`", "do.call(k1, list(i))"),
    ("call: `eval` of a quoted call", "eval(quoted)"),
    ("call: anonymous function in place", "(function(x) x + 1L)(i)"),
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
    ("condition: `tryCatch`, no condition", "tryCatch(i + 1L, error = function(e) 0L)"),
    ("condition: `tryCatch` with `finally`", "tryCatch(i + 1L, finally = NULL)"),
    ("condition: `tryCatch` catches `stop`", 'tryCatch(stop("no"), error = function(e) 0L)'),
    ("condition: `try`, no condition", "try(i + 1L, silent = TRUE)"),
    ("condition: `withCallingHandlers`, no condition", "withCallingHandlers(i + 1L, warning = function(w) 0L)"),
    ("condition: `suppressWarnings`, no condition", "suppressWarnings(i + 1L)"),
    ("condition: `warning` muffled", 'withCallingHandlers(warning("w"), warning = function(w) invokeRestart("muffleWarning"))'),
    ("condition: `signalCondition`, no handler", 'signalCondition(simpleCondition("m"))'),
    ("builtin: `length`", "length(plain)"),
    ("builtin: `is.null`", "is.null(plain)"),
    ("builtin: `[[` by position", "plain[[1L]]"),
    ("builtin: `[[` by name", 'plain[["b"]]'),
    ("builtin: `$`", "plain$b"),
    ("builtin: `[` by position", "numbers[2L]"),
    ("builtin: `[` by name", 'named["b"]'),
    ("builtin: `names`", "names(named)"),
    ("builtin: `c` of two scalars", "c(i, 1L)"),
    ("builtin: `list` with a name", "list(a = i)"),
    ("builtin: `paste0`", 'paste0("a", i)'),
    ("builtin: `sprintf`", 'sprintf("%d", i)'),
    ("builtin: `identical`", "identical(i, 1L)"),
    ("builtin: `is.numeric && length`", "is.numeric(i) && length(i) == 1L"),
    ("builtin: `%in%`", '"beta" %in% words'),
    ("builtin: `nchar`", "nchar(words)"),
    ("builtin: `sum` of 4 numbers", "sum(numbers)"),
    ("builtin: `seq_len`", "seq_len(4L)"),
    ("builtin: `attr`", 'attr(object, "class")'),
    ("builtin: `class`", "class(object)"),
    ("builtin: `unlist` of a short list", "unlist(plain)"),
    ("builtin: `environment()`", "environment()"),
    ("assign: element of a local vector", "{ numbers[2L] <- 9; 0L }"),
    ("assign: field of a local list", "{ plain$a <- i; 0L }"),
    ("assign: `names<-`", '{ names(numbers) <- words[c(1L, 2L, 3L, 1L)]; 0L }'),
    ("assign: `attr<-`", '{ attr(numbers, "unit") <- "m"; 0L }'),
    ("assign: `class<-`", '{ class(plain) <- "widget"; 0L }'),
    ("assign: variable in an environment", "{ holder$value <- i; 0L }"),
]

EMPTY = "empty loop"


def main():
    parser = argparse.ArgumentParser(description="Instructions per R-level operation on ry, under callgrind.")
    parser.add_argument("--ry", default=str(root / "target" / "release" / "ry"))
    parser.add_argument("--filter", help="only operations whose name contains TEXT")
    parser.add_argument("--jit", action="store_true", help="leave the tier-1 JIT on")
    parser.add_argument("--save", metavar="FILE", help="keep the counts in FILE")
    parser.add_argument("--baseline", metavar="FILE", help="compare with the counts an earlier --save kept")
    parser.add_argument("--profile", metavar="NAME", help="the functions holding one operation's instructions")
    parser.add_argument("--top", type=int, default=40, help="rows of --profile")
    args = parser.parse_args()
    if shutil.which("valgrind") is None:
        sys.exit("error: this runs ry under callgrind, and valgrind is not on the PATH")
    ry = os.path.abspath(args.ry)
    if not os.path.exists(ry):
        sys.exit(f"error: no ry binary at {ry}; build one or pass --ry PATH")
    operations = [(EMPTY, "0L")] + [
        operation for operation in OPERATIONS
        if not (args.filter or args.profile) or (args.filter or args.profile) in operation[0]
    ]
    if len(operations) == 1:
        sys.exit("error: no operation matches")
    if args.profile and len(operations) != 2:
        sys.exit(f"error: --profile needs one operation, and {len(operations) - 1} match: "
                 + "; ".join(name for name, _ in operations[1:]))
    with tempfile.TemporaryDirectory(prefix="ry-mechanisms-") as work:
        work = pathlib.Path(work)
        runs = [(index, count) for index in range(len(operations)) for count in (SHORT, LONG)]

        def count(run):
            index, iterations = run
            script = work / f"{index}.R"
            if not script.exists():
                script.write_text(program(operations[index][1]))
            dump = work / f"{index}.{iterations}.callgrind"
            command = ["valgrind", "--tool=callgrind", f"--callgrind-out-file={dump}", "--smc-check=all-non-file",
                       ry, "run", str(script), str(iterations)]
            env = dict(os.environ, OMP_NUM_THREADS="1")
            if not args.jit:
                env["RY_JIT"] = "off"
            done = subprocess.run(command, env=env, capture_output=True, text=True)
            total = re.search(r"Collected : (\d+)", done.stderr)
            if done.returncode != 0 or total is None:
                sys.exit(f"error: {operations[index][0]} failed on ry:\n{done.stderr[-2000:]}")
            return run, int(total[1])

        for index in range(len(operations)):
            (work / f"{index}.R").write_text(program(operations[index][1]))
        with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count()) as pool:
            totals = dict(pool.map(count, runs))
        per_iteration = [(totals[index, LONG] - totals[index, SHORT]) / (LONG - SHORT) for index in range(len(operations))]
        if args.profile:
            print(f"{operations[1][0]}: `{operations[1][1]}`, {per_iteration[1] - per_iteration[0]:,.0f} instructions\n")
            print_profile(work / f"1.{SHORT}.callgrind", work / f"1.{LONG}.callgrind", args.top)
            return
    found = {name: round(per - per_iteration[0], 1) for (name, _), per in zip(operations[1:], per_iteration[1:])}
    baseline = json.load(open(args.baseline))["operations"] if args.baseline else {}
    version = subprocess.run([ry, "--version"], capture_output=True, text=True).stdout.strip()
    print(f"{version}, tier-1 JIT {'on' if args.jit else 'off'}; the empty loop costs {per_iteration[0]:,.0f} instructions an iteration\n")
    header = "| operation | expression | instructions |" + (" baseline | change |" if baseline else "")
    print(header)
    print("| --- | --- | ---: |" + (" ---: | ---: |" if baseline else ""))
    for name, expression in operations[1:]:
        row = f"| {name} | `{expression}` | {found[name]:,.0f} |"
        if baseline:
            before = baseline.get(name)
            row += " | |" if before is None else f" {before:,.0f} | {change(before, found[name])} |"
        print(row)
    if args.save:
        with open(args.save, "w") as file:
            json.dump({"ry": version, "jit": args.jit, "operations": found}, file, indent=1)
            file.write("\n")


def program(expression):
    """The script for one operation: its loop runs inside a closure, so it runs compiled."""
    return (PRELUDE + "\nrun <- function(n) {\n    result <- 0L\n    for (i in seq_len(n)) result <- "
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
