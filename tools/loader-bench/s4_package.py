"""Generates an S4-heavy R package with the shape of a large internal package that a user
reported loading slowly, so its first load can be measured and its S4 semantics cross-checked.

The shape, all at the top level of a file: 583 setClass calls in 6 files (261 with `contains`,
one class defined twice), 380 standardGeneric generics in 1 file, 1879 setMethod calls on 310
generics in 206 files (2 with a two-class signature), 25 setClassUnion calls in 1 file, 4
setValidity, 2 setOldClass, 5 representation() calls, 108 R6 classes in 34 files, 8208
function definitions and 362 other top-level constants, over 590 files. The class graph has
609 classes, 47 of them virtual, inheritance depth at most 5 (261/165/121/51/10/1 classes at
depth 0 to 5), 7132 slots counted with inherited ones, and package generics with at most 63
methods (median 3). Methods also go on as.list (55), show (8), length (6), initialize (5),
c (3), toString (2), print (1) and `[` (1). Bodies hold 874 `new()`, 202 `callNextMethod()` and
498 `slot()` calls, which run only when the code is used.

Four of the ten unions of the package's own classes also take `NULL`, as
`setClassUnion("Family00", c("Cls300", "NULL"))` does, and 11 `setAs` calls coerce between the
package's classes and basic ones, which are the two shapes that used to keep every S4 definition
eager. `--imports` adds real packages to `Imports` and `import()`s them, so the load's import
phase does real work, as `--imports lubridate,data.table,xts` does.

A third of the methods on package generics (`--local-share`) add a `verbose = FALSE` argument
before the generic's `...`, so `setMethod()` wraps them in a `.local` function as the methods
package does for any method whose formals differ from its generic's.

`R/zz_probe.R` defines `s4_probe()`, which exercises dispatch, `is()`, `validObject`,
`callNextMethod`, `show`, `initialize`, unions and the base generics' methods, and returns the
results as a list, so `s4_check.R` can compare a load on one runtime against another.

`--local-share 0.33` gives that share of the package-generic methods an extra formal
(`function(object, verbose = FALSE, ...)`), which `setMethod()` rematches through `.local`, as
about a third of the reported package's methods do.

Usage: python3 s4_package.py ROOT [--seed N] [--imports PKG,PKG] [--local-share F]
"""
import argparse
import os
import random

DEPTHS = [261, 165, 121, 51, 10, 1]
# The two old classes' `.S3Class` slots make the 7132.
SLOT_TOTAL = 7130
# Children of the class with 257 slots inherit them, which is where most slots are.
BIG_CHILDREN = 2
BASIC_SLOT_TYPES = ["numeric", "character", "logical", "integer", "list", "ANY", "function"]
FOREIGN = [("as.list", 55, "x"), ("show", 8, "object"), ("length", 6, "x"),
           ("initialize", 5, ".Object"), ("c", 3, "x"), ("toString", 2, "x"),
           ("print", 1, "x"), ("[", 1, "x")]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("root")
    parser.add_argument("--seed", type=int, default=20261102)
    parser.add_argument("--name", default="s4heavy")
    parser.add_argument("--imports", default="")
    parser.add_argument("--local-share", type=float, default=0.33,
                        help="share of package-generic methods whose formals add an argument")
    args = parser.parse_args()
    rng = random.Random(args.seed)
    package = Package(rng)
    # A separate stream, so the share leaves every other choice of a given seed as it was.
    package.local_rng = random.Random(args.seed + 1)
    package.local_share = args.local_share
    package.write(args.root, args.name, [name for name in args.imports.split(",") if name])


class Klass:
    def __init__(self, index, depth, parent, virtual):
        self.name = f"Cls{index:03d}"
        self.depth = depth
        self.parent = parent
        self.virtual = virtual
        self.own_slots = []
        self.union_only = False
        self.validity = False
        self.representation = False
        self.children = []

    def all_slots(self):
        inherited = self.parent.all_slots() if self.parent else []
        return inherited + self.own_slots


class Package:
    def __init__(self, rng):
        self.rng = rng
        self.build_classes()
        self.build_generics()
        self.build_methods()

    # Classes: 582 distinct ones, plus the 25 unions and 2 old classes that make 609. A class
    # that is only a union member is at depth 1 without `contains`.
    def build_classes(self):
        rng = self.rng
        roots = DEPTHS[0] - 25 - 1  # unions and the first old class are depth-0 classes too
        depth_one = DEPTHS[1] - 1  # the second old class extends the first
        union_only = depth_one - 78
        self.classes = []
        index = 0
        virtual_roots = []
        for position in range(roots):
            klass = Klass(index, 0, None, position < 20)
            index += 1
            self.classes.append(klass)
            if klass.virtual:
                virtual_roots.append(klass)
        concrete_roots = [klass for klass in self.classes if not klass.virtual]
        for position in range(union_only):
            klass = Klass(index, 1, None, False)
            klass.union_only = True
            index += 1
            self.classes.append(klass)
        previous = virtual_roots + concrete_roots
        for depth, count in enumerate(DEPTHS[1:], start=1):
            if depth == 1:
                count = 78
            level = []
            for position in range(count):
                if depth == 1 and position < len(virtual_roots):
                    parent = virtual_roots[position]
                elif depth == 1 and position < len(virtual_roots) + BIG_CHILDREN:
                    parent = concrete_roots[3]
                else:
                    parent = rng.choice(previous if depth > 1 else concrete_roots)
                klass = Klass(index, depth, parent, False)
                index += 1
                parent.children.append(klass)
                level.append(klass)
                self.classes.append(klass)
            previous = level
        assert len(self.classes) == 582
        assert sum(1 for klass in self.classes if klass.parent) == 261
        slot_index = 0

        # A slot may name an earlier class, which `new()` then fills with that class's
        # prototype; a later one would be undefined when the slot is declared.
        def new_slot(owner):
            nonlocal slot_index
            slot_index += 1
            kind = rng.choice(BASIC_SLOT_TYPES)
            earlier = [klass for klass in concrete_roots[:40] if klass.name < owner.name]
            if earlier and rng.random() < 0.08:
                kind = rng.choice(earlier).name
            return (f"s{slot_index}", kind)

        big = concrete_roots[3]
        for klass in self.classes:
            if klass.virtual:
                continue
            count = 257 if klass is big else rng.randint(0, 2) if klass.parent else rng.randint(3, 10)
            klass.own_slots = [new_slot(klass) for _ in range(count)]
        # Leaf roots count their slots once, so adding or removing one of theirs moves the
        # total, inherited slots included, by exactly one. Slots go to leaves above the median
        # and come from leaves below it, which keeps the median where it is.
        excess = sum(len(klass.all_slots()) for klass in self.classes) - SLOT_TOTAL
        leaves = [klass for klass in concrete_roots if not klass.children and klass is not big]
        while excess != 0:
            klass = rng.choice(leaves)
            if excess > 0 and 2 < len(klass.own_slots) < 7:
                klass.own_slots.pop()
                excess -= 1
            elif excess < 0 and 7 < len(klass.own_slots) < 24:
                klass.own_slots.append(new_slot(klass))
                excess += 1
        for klass in rng.sample([k for k in self.classes if not k.virtual and k.own_slots], 4):
            klass.validity = True
        for klass in rng.sample([k for k in concrete_roots if not k.children and k is not big], 5):
            klass.representation = True
        self.duplicate = rng.choice([k for k in concrete_roots if not k.children and not k.validity
                                     and not k.representation and k is not big])
        members = [klass for klass in self.classes if klass.union_only]
        rng.shuffle(members)
        self.unions = []
        basic = [["numeric", "NULL"], ["character", "NULL"], ["logical", "NULL"], ["list", "NULL"],
                 ["integer", "NULL"], ["function", "NULL"], ["numeric", "character"],
                 ["numeric", "logical"], ["character", "logical"], ["list", "character"],
                 ["Date", "NULL"], ["character", "integer", "NULL"], ["data.frame", "NULL"],
                 ["matrix", "NULL"], ["environment", "NULL"]]
        for position, member_classes in enumerate(basic):
            self.unions.append((f"Basic{position:02d}OrNull", member_classes))
        for position in range(10):
            share = [klass.name for klass in members[position::10]]
            # A union of the package's own classes and a basic one, as `c("Cls300", "NULL")`.
            if position < 4:
                share.append("NULL")
            self.unions.append((f"Family{position:02d}", share))
        self.virtual_count = 20 + 25 + 2

    # Generics: 380 package generics, of which 302 get methods; methods are spread with a
    # heavy tail so the largest has 63 and the median is 3.
    def build_generics(self):
        rng = self.rng
        self.generics = [f"gen_{index:03d}" for index in range(380)]
        self.pair_generics = {self.generics[0], self.generics[1]}
        with_methods = self.generics[:302]
        counts = sorted((max(1, round(rng.lognormvariate(1.1, 0.9))) for _ in with_methods),
                        reverse=True)
        counts[0] = 63
        counts = [min(count, 63) for count in counts]
        target = 1879 - sum(count for _, count, _ in FOREIGN)
        # The heavy tail takes up the difference, so the median stays where the draw put it.
        position = 1
        while sum(counts) != target:
            if sum(counts) < target and counts[position] < 62:
                counts[position] += 1
            elif sum(counts) > target and counts[position] > 1:
                counts[position] -= 1
            position = position % 40 + 1
        counts.sort(reverse=True)
        self.method_counts = dict(zip(with_methods, counts))

    def build_methods(self):
        rng = self.rng
        concrete = [klass for klass in self.classes if not klass.virtual]
        self.methods = []
        for generic, count in self.method_counts.items():
            for klass in rng.sample(concrete, count):
                self.methods.append((generic, [klass], None))
        for generic, count, argument in FOREIGN:
            chosen = rng.sample([klass for klass in concrete if not klass.children
                                 and klass is not self.duplicate], count)
            for klass in chosen:
                self.methods.append((generic, [klass], argument))
        # Two methods whose signature names two classes.
        pairs = 0
        for position, (generic, signature, argument) in enumerate(self.methods):
            if generic in self.pair_generics and pairs < 2:
                self.methods[position] = (generic, signature + [rng.choice(concrete)], argument)
                pairs += 1
        rng.shuffle(self.methods)
        assert len(self.methods) == 1879

    def coercions(self):
        """11 `setAs` calls: to `character` and from `list` for classes whose prototype is
        valid, and between two unrelated classes, one with a `replace` method."""
        rng = self.rng
        plain = [klass for klass in self.classes if not klass.virtual and not klass.children
                 and not klass.parent and not klass.validity and klass is not self.duplicate]
        chosen = rng.sample(plain, 12)
        calls = []
        for klass in chosen[:4]:
            calls.append(f'setAs("{klass.name}", "character", function(from) "{klass.name}")\n')
        for klass in chosen[4:8]:
            calls.append(f'setAs("list", "{klass.name}", function(from) new("{klass.name}"))\n')
        for source, target in zip(chosen[8:10], chosen[10:12]):
            calls.append(f'setAs("{source.name}", "{target.name}", function(from) new("{target.name}"))\n')
        source, target = chosen[11], chosen[8]
        calls.append(f'setAs("{source.name}", "{target.name}", function(from) new("{target.name}"),\n'
                     f'  replace = function(from, value) from)\n')
        self.coerced = chosen
        return calls

    def write(self, root, name, imports):
        rng = self.rng
        os.makedirs(os.path.join(root, "R"), exist_ok=True)
        files = {}
        collate = []

        def add(file_name, text):
            files.setdefault(file_name, []).append(text)
            if file_name not in collate:
                collate.append(file_name)

        add("aaa_constants.R", "PACKAGE_TAG <- \"s4heavy\"\n")
        # Six class files; the class graph is in creation order, so parents come first.
        per_file = -(-len(self.classes) // 6)
        for position, klass in enumerate(self.classes):
            file_name = f"classes_{position // per_file + 1}.R"
            if position == 0:
                add(file_name, 'setOldClass("ledger_s3")\nsetOldClass(c("ledger_s3_sub", "ledger_s3"))\n')
            add(file_name, class_text(klass))
            if klass.validity:
                add(file_name, validity_text(klass))
        add("classes_6.R", class_text(self.duplicate))
        unions = "".join(
            f'setClassUnion("{union}", c({", ".join(quote(member) for member in members)}))\n'
            for union, members in self.unions)
        add("classes_unions.R", unions)
        add("coercions.R", "".join(self.coercions()))
        generics = []
        for generic in self.generics:
            formals = "x, y, ..." if generic in self.pair_generics else "object, ..."
            generics.append(f'setGeneric("{generic}", function({formals}) standardGeneric("{generic}"))\n')
        add("generics.R", "".join(generics))
        method_files = [f"methods_{index:03d}.R" for index in range(206)]
        for file_name in method_files:
            add(file_name, f"# Methods.\n")
        counters = {"new": 0, "generic_next": 0, "slot": 0}
        for position, (generic, signature, argument) in enumerate(self.methods):
            file_name = method_files[position % 206]
            add(file_name, method_text(rng, generic, signature, argument, self, counters))
        function_files = [f"functions_{index:03d}.R" for index in range(374)]
        r6_files = function_files[:34]
        all_code_files = method_files + function_files
        # 8208 functions: what is left of the bodies' new() and slot() calls goes here.
        concrete = [klass for klass in self.classes if not klass.virtual]
        new_left = 874 - counters["new"]
        slot_left = 498 - counters["slot"]
        for index in range(8208):
            file_name = all_code_files[index % len(all_code_files)]
            lines = [f"fn_{index:05d} <- function(x, scale = {rng.randint(1, 9)}) {{",
                     "  x <- as.numeric(x) * scale",
                     "  y <- vapply(x, function(v) v + scale, numeric(1))"]
            if new_left > 0 and index % 9 == 0:
                klass = rng.choice(concrete)
                lines.append(f'  obj <- new("{klass.name}")')
                new_left -= 1
                if slot_left > 0 and klass.all_slots():
                    lines.append(f'  y <- c(y, length(slot(obj, "{klass.all_slots()[0][0]}")))')
                    slot_left -= 1
            if index % 11 == 0 and index > 0:
                lines.append(f"  y <- c(y, fn_{index - 1:05d}(1))")
            lines += ["  list(total = sum(y), n = length(y))", "}\n"]
            add(file_name, "\n".join(lines))
        assert new_left == 0 and slot_left == 0, (new_left, slot_left)
        for index in range(108):
            file_name = r6_files[index % 34]
            add(file_name, (
                f'Store{index:03d} <- R6Class("Store{index:03d}", public = list(\n'
                "  items = NULL,\n"
                "  add = function(x) { self$items <- c(self$items, x); invisible(self) },\n"
                "  size = function() length(self$items)\n"
                "))\n"))
        constant_files = function_files + method_files
        for index in range(361):
            add(constant_files[index % len(constant_files)],
                f'CONST_{index:03d} <- list(level = {index}, name = "c{index}", weights = c(a = 0.5, b = {index % 7}))\n')
        add("zz_probe.R", probe_text(self))
        for file_name, chunks in files.items():
            with open(os.path.join(root, "R", file_name), "w") as handle:
                handle.write("\n".join(chunks))
        with open(os.path.join(root, "DESCRIPTION"), "w") as handle:
            handle.write(
                f"Package: {name}\nTitle: Synthetic S4-Heavy Package\nVersion: 0.1.0\n"
                "Description: Generated by tools/loader-bench/s4_package.py.\n"
                "License: MIT\nEncoding: UTF-8\n"
                f"Imports: {', '.join(['methods', 'R6'] + imports)}\n"
                "Collate:\n" + "".join(f"    '{file_name}'\n" for file_name in collate))
        with open(os.path.join(root, "NAMESPACE"), "w") as handle:
            handle.write('import(methods)\nimportFrom(R6, R6Class)\n'
                         + "".join(f"import({package})\n" for package in imports) +
                         'exportPattern("^[[:alpha:]]")\nexportClassPattern("^[[:alpha:]]")\n'
                         "exportMethods(" + ", ".join(sorted({quote(generic) for generic, _, _ in self.methods}
                                                             | {'"chain_probe"', '"mixed_probe"'})) + ")\n")


def quote(text):
    return f'"{text}"'


def class_text(klass):
    if klass.virtual:
        return f'setClass("{klass.name}", representation = "VIRTUAL")\n'
    slots = ", ".join(f"{slot} = {quote(kind)}" for slot, kind in klass.own_slots)
    if klass.representation:
        return f'setClass("{klass.name}", representation({slots}))\n'
    parts = [quote(klass.name), f"slots = c({slots})" if slots else None]
    if klass.parent:
        parts.append(f"contains = {quote(klass.parent.name)}")
    defaults = [(slot, kind) for slot, kind in klass.own_slots if kind in ("numeric", "character")][:2]
    if defaults:
        values = ", ".join(f"{slot} = {'0' if kind == 'numeric' else quote('none')}" for slot, kind in defaults)
        parts.append(f"prototype = list({values})")
    return f"setClass({', '.join(part for part in parts if part)})\n"


def validity_text(klass):
    slot = klass.all_slots()[0][0]
    return (f'setValidity("{klass.name}", function(object) {{\n'
            f'  if (length(object@{slot}) > 3) "{slot} is too long" else TRUE\n}})\n')


def method_text(rng, generic, signature, argument, package, counters):
    klass = signature[0]
    signature_text = (quote(klass.name) if len(signature) == 1
                      else f"signature({', '.join(quote(k.name) for k in signature)})")
    slots = klass.all_slots()
    if argument is None:
        formals = "x, y, ..." if generic in package.pair_generics else "object, ..."
        receiver = "x" if generic in package.pair_generics else "object"
        # A formal the generic lacks makes `setMethod()` wrap the body in `.local`.
        if package.local_rng.random() < package.local_share:
            formals = formals.replace("...", "verbose = FALSE, ...")
    elif generic == "initialize":
        return (f'setMethod("initialize", {signature_text}, function(.Object, ...) {{\n'
                "  .Object <- callNextMethod(.Object, ...)\n"
                f'  .Object@{slots[0][0]} <- {default_for(slots[0][1])}\n'
                "  .Object\n})\n")
    elif generic == "show":
        return (f'setMethod("show", {signature_text}, function(object) {{\n'
                f'  cat("<{klass.name}", length(slotNames(object)), "slots>\\n")\n}})\n')
    elif generic == "length":
        return f'setMethod("length", {signature_text}, function(x) {len(slots)}L)\n'
    elif generic == "c":
        return f'setMethod("c", {signature_text}, function(x, ...) list(x, ...))\n'
    elif generic == "[":
        counters["slot"] += 1
        return f'setMethod("[", {signature_text}, function(x, i, ...) slot(x, slotNames(x)[[i]]))\n'
    elif generic == "toString":
        return f'setMethod("toString", {signature_text}, function(x, ...) "{klass.name}")\n'
    elif generic == "print":
        return f'setMethod("print", {signature_text}, function(x, ...) cat("print {klass.name}\\n"))\n'
    else:  # as.list
        counters["slot"] += 1
        return (f'setMethod("as.list", {signature_text}, function(x, ...) {{\n'
                "  sapply(slotNames(x), function(name) slot(x, name), simplify = FALSE)\n})\n")
    lines = [f'setMethod("{generic}", {signature_text}, function({formals}) {{',
             f'  tag <- "{generic}:{klass.name}"']
    if slots:
        lines.append(f"  first <- {receiver}@{slots[0][0]}")
    if counters["slot"] < 400 and slots and rng.random() < 0.3:
        lines.append(f'  second <- slot({receiver}, "{rng.choice(slots)[0]}")')
        counters["slot"] += 1
    if counters["new"] < 500 and rng.random() < 0.27:
        target = rng.choice([k for k in package.classes if not k.virtual])
        lines.append(f'  made <- new("{target.name}")')
        counters["new"] += 1
    # Five of the 202 calls are in the initialize methods.
    if counters["generic_next"] < 197 and klass.parent is not None and rng.random() < 0.5:
        lines.append("  base <- tryCatch(callNextMethod(), error = function(e) NULL)")
        counters["generic_next"] += 1
    lines += ["  tag", "})\n"]
    return "\n".join(lines)


def default_for(kind):
    return {"numeric": "1", "character": '"set"', "logical": "TRUE", "integer": "1L",
            "list": "list(1)", "function": "identity"}.get(kind, "1")


def probe_text(package):
    rng = random.Random(7)
    chain = [klass for klass in package.classes if klass.depth == 5]
    while chain[-1].parent is not None:
        chain.append(chain[-1].parent)
    chain = list(reversed(chain))
    chain_methods = []
    for position, klass in enumerate(chain):
        body = (f'"{klass.name}"' if position == 0
                else f'c("{klass.name}", callNextMethod())')
        chain_methods.append(f'setMethod("chain_probe", "{klass.name}", function(object, ...) {body})')
    concrete = [klass for klass in package.classes if not klass.virtual]
    sample = rng.sample(concrete, 60)
    by_generic = {}
    for generic, signature, argument in package.methods:
        if argument is None and len(signature) == 1:
            by_generic.setdefault(generic, []).append(signature[0])
    dispatch = []
    for generic in rng.sample(sorted(by_generic), 40):
        receiver = rng.choice(by_generic[generic])
        dispatch.append((generic, receiver))
        # A subclass without its own method inherits the receiver's.
        for child in receiver.children[:1]:
            if all(child is not other for other in by_generic[generic]):
                dispatch.append((generic, child))
    foreign = {}
    for generic, signature, argument in package.methods:
        if argument is not None:
            foreign.setdefault(generic, []).append(signature[0])
    validity = [klass for klass in package.classes if klass.validity]
    mixed = package.unions[15][0]
    lines = ['setGeneric("chain_probe", function(object, ...) standardGeneric("chain_probe"))',
             'setGeneric("mixed_probe", function(object, ...) standardGeneric("mixed_probe"))',
             f'setMethod("mixed_probe", "{mixed}", function(object, ...) "{mixed}")',
             'setMethod("mixed_probe", "ANY", function(object, ...) "ANY")']
    lines += chain_methods + ["", "s4_probe <- function() {", "  out <- list()"]
    lines.append(f'  out$chain <- chain_probe(new("{chain[-1].name}"))')
    lines.append("  classes <- c(" + ", ".join(quote(k.name) for k in sample) + ")")
    lines.append("  out$is_parent <- vapply(classes, function(cls) { def <- getClass(cls); "
                 "c(length(def@contains), length(slotNames(cls)), isVirtualClass(cls)) |> paste(collapse = \"/\") }, \"\")")
    lines.append("  out$extends <- lapply(classes, extends)")
    lines.append("  out$is_root <- vapply(classes, function(cls) is(new(cls), extends(cls)[length(extends(cls))]), NA)")
    lines.append("  out$dispatch <- c(" + ", ".join(
        f'{generic}(new("{klass.name}"))' for generic, klass in dispatch) + ")")
    lines.append("  out$exists <- c(" + ", ".join(
        f'existsMethod("{generic}", "{klass.name}")' for generic, klass in dispatch) + ")")
    for klass in validity:
        slot, kind = klass.all_slots()[0]
        bad = {"numeric": "1:5 + 0", "character": "letters[1:5]", "logical": "rep(TRUE, 5)",
               "integer": "1:5", "list": "as.list(1:5)", "function": None, "ANY": "1:5"}.get(kind, None)
        if bad is None:
            continue
        lines.append(f'  out$valid_{klass.name} <- tryCatch(validObject(new("{klass.name}", {slot} = {bad})), '
                     "error = conditionMessage)")
        lines.append(f'  out$valid_ok_{klass.name} <- validObject(new("{klass.name}"))')
    for generic, receivers in sorted(foreign.items()):
        for klass in receivers[:3]:
            call = {"as.list": "names(as.list(obj))", "show": "capture.output(show(obj))",
                    "length": "length(obj)", "initialize": "slot(obj, slotNames(obj)[[1]])",
                    "c": "length(c(obj, obj))", "toString": "toString(obj)",
                    "print": "capture.output(print(obj))", "[": "class(obj[1])"}[generic]
            lines.append(f'  obj <- new("{klass.name}"); out[["{generic}_{klass.name}"]] <- {call}')
    union_name, members = package.unions[15]
    lines.append(f'  out$union <- c(is(new("{members[0]}"), "{union_name}"), isVirtualClass("{union_name}"), '
                 f'extends("{members[0]}", "{union_name}"))')
    lines.append(f'  out$basic_union <- is(1, "{package.unions[0][0]}")')
    mixed, mixed_members = package.unions[15]
    lines.append(f'  out$mixed_union <- c(is(NULL, "{mixed}"), extends("NULL", "{mixed}"), '
                 f'is(new("{mixed_members[0]}"), "{mixed}"), isVirtualClass("{mixed}"), '
                 f'mixed_probe(NULL), mixed_probe(new("{mixed_members[0]}")))')
    coerced = package.coerced
    lines.append(f'  out$as_character <- as(new("{coerced[0].name}"), "character")')
    lines.append(f'  out$as_list <- class(as(list(1), "{coerced[4].name}"))')
    lines.append(f'  out$as_class <- class(as(new("{coerced[8].name}"), "{coerced[10].name}"))')
    lines.append(f'  out$as_exists <- c(existsMethod("coerce", c("{coerced[0].name}", "character")), '
                 f'existsMethod("coerce", c("list", "{coerced[4].name}")), '
                 f'existsMethod("coerce<-", c("{coerced[11].name}", "{coerced[8].name}")))')
    lines.append('  out$show_methods <- capture.output(showMethods("gen_000"))')
    lines.append('  out$old_class <- extends("ledger_s3_sub", "ledger_s3")')
    lines.append(f'  out$autoprint <- capture.output(new("{foreign["show"][0].name}"))')
    lines.append("  out$n_classes <- length(getClasses(topenv()))")
    lines.append("  out$n_generics <- length(getGenerics(topenv()))")
    lines.append("  out")
    lines.append("}\n")
    return "\n".join(lines)


if __name__ == "__main__":
    main()
