"""Generates a synthetic R package for loader benchmarks: helper functions, an S3 class with
registered methods per file, an R6 class in every fifth file, and an S4 class, generic and
method in every tenth.

Usage: python3 generate.py NAME FILES FUNCTIONS_PER_FILE ROOT
"""
import os, random, sys

name, files, functions_per_file, root = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
random.seed(7)
os.makedirs(os.path.join(root, "R"), exist_ok=True)
namespace = ["importFrom(R6, R6Class)", "import(methods)", "importFrom(stats, median, sd)", "importFrom(utils, head)"]
body_lines = [
    "  x <- as.numeric(x)",
    "  y <- x * {k} + length(x)",
    "  if (length(y) > {k}) y <- head(y, {k})",
    "  z <- vapply(y, function(v) v + {k}, numeric(1))",
    "  s <- sum(z, na.rm = TRUE) / max(1, length(z))",
    "  out <- list(value = s, n = length(z), label = paste0(\"f{k}\", s))",
    "  for (i in seq_len(min(3L, length(z)))) out$value <- out$value + z[[i]]",
    "  stopifnot(is.list(out))",
]
for f in range(files):
    lines = [f"# File {f}: generated.", ""]
    for g in range(functions_per_file):
        fn = f"f{f}_{g}"
        k = random.randint(1, 9)
        lines.append(f"#' Compute something {f}.{g}.")
        lines.append(f"{fn} <- function(x, scale = {k}) {{")
        for line in body_lines:
            lines.append(line.replace("{k}", str(k)))
        if g > 0:
            lines.append(f"  out$prev <- f{f}_{g-1}")
        lines.append("  out")
        lines.append("}")
        lines.append("")
        if g % 10 == 0:
            namespace.append(f"export({fn})")
    cls = f"cls{f}"
    lines += [
        f"new_{cls} <- function(x) structure(list(x = x), class = \"{cls}\")",
        f"print.{cls} <- function(x, ...) {{ cat(\"<{cls}>\", format(x), \"\\n\"); invisible(x) }}",
        f"format.{cls} <- function(x, ...) paste0(\"{cls}:\", length(x$x))",
        "",
        f"DEFAULTS_{f} <- list(level = {f}, name = toupper(\"{cls}\"))",
        "",
    ]
    if f % 5 == 0:
        lines += [
        f"Store{f} <- R6Class(\"Store{f}\", public = list(",
        "  items = NULL,",
        f"  add = function(x) {{ self$items <- c(self$items, f{f}_0(x)$value); invisible(self) }},",
        "  size = function() length(self$items)",
        "))",
        "",
        ]
        namespace.append(f"export(Store{f})")
    if f % 10 == 0:
        lines += [
        f"setClass(\"Shape{f}\", representation(size = \"numeric\"))",
        f"setGeneric(\"area{f}\", function(shape) standardGeneric(\"area{f}\"))",
        f"setMethod(\"area{f}\", \"Shape{f}\", function(shape) shape@size^2)",
        "",
        ]
    namespace += [f"S3method(print, {cls})", f"S3method(format, {cls})", f"export(new_{cls})"]
    with open(os.path.join(root, "R", f"file{f:04d}.R"), "w") as out:
        out.write("\n".join(lines))
with open(os.path.join(root, "NAMESPACE"), "w") as out:
    out.write("\n".join(namespace) + "\n")
with open(os.path.join(root, "DESCRIPTION"), "w") as out:
    out.write(f"Package: {name}\nVersion: 1.0.0\nTitle: Synthetic\nDescription: Synthetic.\nLicense: MIT\nImports: R6, methods, stats, utils\nEncoding: UTF-8\n")
