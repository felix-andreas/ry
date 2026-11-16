# Kernels modeled on a user's profiles of a package's test suite: each is one code shape that a
# profile ranked high, cut down to a loop, with every name replaced by a neutral one. K1 to K5,
# K12 and K13 follow shapes a user sent as code. K6 to K11, K14 and K15 are reconstructions from
# one-line descriptions, and so are the sizes and the runner. `set.seed` and the `checks` list
# make the data and one result per kernel comparable between GNU R and ry.
#
# Usage: [KERNELS=k1,k3] [SCALE=1] Rscript kernels.R
# Prints one line per kernel, `kernel  seconds  checksum`: the time of one run after a warm-up run
# of a twentieth the size, and a checksum of one call's result.

suppressPackageStartupMessages({ library(methods); library(data.table); library(xts) })
set.seed(20240102)

setClass("Record", slots = c(a = "character", b = "character", c = "character",
                          d = "character", e = "integer", f = "integer"))
invisible(setValidity("Record", function(object) {
  fields <- list(a = object@a, b = object@b, c = object@c, d = object@d, e = object@e, f = object@f)
  if (length(unique(purrr::map_int(fields, length))) != 1) "slots differ in length" else TRUE
}))
makeRecord <- function(a = NA_character_, b = NA_character_, c = NA_character_,
                    d = NA_character_, e = NA_integer_, f = NA_integer_)
  new("Record", a = a, b = b, c = c, d = d, e = e, f = f)
recordLabel <- function(k, useB = TRUE, useC = TRUE, useD = FALSE, useE = FALSE, useF = TRUE) {
  args <- list(k@a, if (useB) k@b, if (useC) k@c, if (useD) k@d, if (useE) k@e, if (useF) k@f,
               sep = "|") |> purrr::compact()
  paste |> do.call(args)
}
k1 <- function(n) for (i in seq_len(n)) recordLabel(makeRecord(a = "id1", b = "p", c = "q"))

toDate <- function(x, ...) zoo::as.Date(x, ...)
k2 <- function(n) { d <- as.Date("2024-01-02"); for (i in seq_len(n)) { toDate("2024-03-15"); toDate(d) } }

isBlank <- function(x) {
  if (isS4(x)) return(FALSE)
  if (data.table::is.data.table(x)) return(ifelse(length(x) > 1, all(is.na(x)), TRUE))
  if (class(x)[1] == "character")
    return(ifelse(length(x) > 1, all(is.na(x) | (x == "NA")), !isTRUE(!(is.na(unname(x)) || (x == "NA")))))
  ifelse(length(x) > 1, all(is.na(x)), !isTRUE(!is.na(unname(x))))
}
k3 <- function(n) for (i in seq_len(n)) { isBlank("abc"); isBlank(NA_integer_); isBlank(3.5) }

argumentNames <- function(...) vapply(as.list(substitute(list(...)))[-1], deparse, "")
checkClasses <- function(objects, types, labels) {
  for (i in seq_along(objects)) {
    o <- objects[[i]]
    if (!identical(class(o)[1], types[[i]]) && !is(o, types[[i]])) stop("bad type ", labels[i])
  }
  invisible(TRUE)
}
checkArguments <- function(..., types = NULL, env = NULL) {
  objects <- list(...)
  if (is.null(types)) return(invisible(TRUE))
  delayedAssign("labels", argumentNames(...))
  checkClasses(objects, types, labels)
  if (is.null(env)) env <- parent.frame()
  invisible(TRUE)
}
k4 <- function(n) { x <- "a"; y <- 1L; z <- Sys.Date()
  for (i in seq_len(n)) checkArguments(x, y, z, types = c("character", "numeric", "Date")) }

setClass("Shelf", slots = c(s1 = "list", s2 = "list", s3 = "list"))
idx <- as.Date("2020-01-01") + 0:499
seriesList <- function(p) setNames(lapply(1:20, function(j) xts(rnorm(500), idx)), paste0(p, 1:20))
shelf <- new("Shelf", s1 = seriesList("A"), s2 = seriesList("B"), s3 = seriesList("C"))
onShelf <- function(object, key) {
  slots <- lapply(slotNames(object), function(s) slot(object, s))
  held <- as.character(unlist(lapply(slots, names), use.names = FALSE))
  paste(key, "q", sep = "|") %in% paste(held, "q", sep = "|")
}
k5 <- function(n) for (i in seq_len(n)) onShelf(shelf, c("A3", "B7", "Z1"))

xl <- lapply(1:8, function(j) xts(rnorm(250), sort(sample(idx, 250))))

# Reconstructed: "do.call(xts::merge.xts, c(list_of_8_xts, all = TRUE, fill = zoo::na.locf))".
mergeSeries <- function(xs) do.call(xts::merge.xts, c(xs, all = TRUE, fill = zoo::na.locf))
k6 <- function(n) for (i in seq_len(n)) mergeSeries(xl)

# Reconstructed: "acc <- rbind(acc, data.table(...), fill = TRUE) in a loop".
growTable <- function(rows) {
  acc <- data.table()
  for (j in seq_len(rows))
    acc <- rbind(acc, data.table(id = j, name = paste0("n", j), value = j * 1.5), fill = TRUE)
  acc
}
k7 <- function(n) for (i in seq_len(n)) growTable(10L)

# Reconstructed: "small matrix arithmetic on column slices with pmin and pmax".
grid <- matrix(rnorm(60, 0.02, 0.01), nrow = 12, dimnames = list(NULL, paste0("c", 1:5)))
bound <- function(m, floor = 0, cap = 0.03) {
  low <- m[, 1:2, drop = FALSE]
  high <- m[, 3:4, drop = FALSE]
  pmax(pmin(low * 1.1 + high, cap), floor) - pmin(low, high) / (1 + pmax(abs(high), m[, 5]))
}
k8 <- function(n) for (i in seq_len(n)) bound(grid)

# K9 to K11 come from a second profile, of a session that sources a package's files: `parse`,
# `readLines` and `list.files` per call. The files are generated here, forty of about 120 lines.
sourceDir <- file.path(tempdir(), "bench-ci-sources")
dir.create(file.path(sourceDir, "sub"), recursive = TRUE, showWarnings = FALSE)
sourceFiles <- vapply(1:40, function(j) {
  path <- file.path(sourceDir, if (j %% 4 == 0) "sub" else "", sprintf("file%02d.R", j))
  blocks <- vapply(1:12, function(b) paste(c(
    sprintf("#' Computes step %d of file %d.", b, j),
    sprintf("step_%d_%d <- function(x, scale = %d, ..., label = \"s%d\") {", j, b, b, b),
    "  if (is.null(x) || length(x) == 0L) return(invisible(NULL))",
    "  total <- 0",
    "  for (value in x) total <- total + value * scale",
    sprintf("  out <- list(total = total, label = paste0(label, \"-\", %d), ok = total > 0)", j),
    "  structure(out, class = c(\"step\", \"list\"))",
    "}", ""), collapse = "\n"), "")
  writeLines(blocks, path)
  normalizePath(path)
}, "")
k9 <- function(n) for (i in seq_len(n)) for (path in sourceFiles) parse(path, keep.source = TRUE)
k10 <- function(n) for (i in seq_len(n)) for (path in sourceFiles) readLines(path, warn = FALSE)
k11 <- function(n) for (i in seq_len(n))
  list.files(sourceDir, pattern = "\\.[Rr]$", recursive = TRUE, full.names = TRUE)

# K12 and K13 follow a reproducer a user sent for `new()`: a class of two plain slots, and a
# class whose prototype holds a million doubles, which `new()` must not copy.
setClass("Plain", representation(a = "numeric", b = "character"))
setClass("Seeded", representation(series = "numeric", name = "character"),
         prototype(series = runif(1e6), name = "x"))
k12 <- function(n) for (i in seq_len(n)) new("Plain", a = 1, b = "x")
k13 <- function(n) for (i in seq_len(n)) new("Seeded", name = "y")

# K14 comes from the same profile as K9 to K11, where reading objects out of installed packages'
# lazy-load databases ranked high: it binds every object of three packages as a promise and
# forces each one, which reads, inflates and unserializes its record.
lazyBases <- vapply(c("zoo", "xts", "data.table"),
                    function(package) file.path(system.file("R", package = package), package), "")
fetchAll <- function(base) {
  env <- new.env()
  lazyLoad(base, envir = env)
  names <- ls(env, all.names = TRUE)
  functions <- 0L
  for (name in names) if (is.function(get(name, envir = env))) functions <- functions + 1L
  c(length(names), functions)
}
k14 <- function(n) for (i in seq_len(n)) for (base in lazyBases) fetchAll(base)

# Reconstructed: "a grouped data.table `j` calling a small function, many small groups". GForce
# cannot optimize a call to a user's function, so data.table's C code evaluates `j` once per group.
grouped <- data.table(g = rep(1:4000, each = 5L), v = rnorm(20000), w = runif(20000))
spread <- function(x) max(x) - min(x)
groupSummary <- function(table) table[, .(s = spread(v), t = v[1L] + w[.N]), by = g]
k15 <- function(n) for (i in seq_len(n)) groupSummary(grouped)

sizes <- c(k1 = 20000, k2 = 30000, k3 = 100000, k4 = 60000, k5 = 20000, k6 = 4000, k7 = 500, k8 = 30000,
           k9 = 20, k10 = 100, k11 = 2000, k12 = 100000, k13 = 500, k14 = 10, k15 = 40)
checks <- list(
  k1 = function() recordLabel(makeRecord(a = "id1", b = "p", c = "q")),
  k2 = function() format(c(toDate("2024-03-15"), toDate(as.Date("2024-01-02")))),
  k3 = function() c(isBlank("abc"), isBlank(NA_integer_), isBlank(3.5), isBlank("NA")),
  k4 = function() { x <- "a"; y <- 1L; z <- Sys.Date()
    c(checkArguments(x, y, z, types = c("character", "numeric", "Date")),
      tryCatch(checkArguments(x, y, z, types = c("character", "character", "Date")), error = conditionMessage)) },
  k5 = function() onShelf(shelf, c("A3", "B7", "Z1")),
  k6 = function() { merged <- mergeSeries(xl); c(dim(merged), round(sum(merged, na.rm = TRUE), 6)) },
  k7 = function() { acc <- growTable(10L); c(dim(acc), sum(acc$value), nchar(paste(acc$name, collapse = ""))) },
  k8 = function() round(sum(bound(grid)), 10),
  k9 = function() { parsed <- lapply(sourceFiles, parse, keep.source = TRUE)
    c(sum(lengths(parsed)), sum(nchar(unlist(lapply(parsed, function(e) as.character(attr(e, "srcref")))))),
      nrow(getParseData(parsed[[1]]))) },
  k10 = function() sum(nchar(unlist(lapply(sourceFiles, readLines)))),
  k11 = function() basename(list.files(sourceDir, pattern = "\\.[Rr]$", recursive = TRUE, full.names = TRUE))[c(1, 20, 40)],
  k12 = function() { object <- new("Plain", a = 1, b = "x"); c(object@a, nchar(object@b), length(slotNames(object))) },
  k13 = function() { first <- new("Seeded", name = "y"); first@series[1] <- -1; second <- new("Seeded")
    c(length(first@series), first@series[1], second@series[1] >= 0, nchar(paste0(first@name, second@name)),
      round(sum(getClass("Seeded")@prototype@series), 6)) },
  k14 = function() vapply(lazyBases, fetchAll, c(0L, 0L), USE.NAMES = FALSE),
  k15 = function() { summary <- groupSummary(grouped); c(nrow(summary), round(sum(summary$s), 6), round(sum(summary$t), 6)) }
)

selected <- Sys.getenv("KERNELS", "")
selected <- if (nzchar(selected)) strsplit(selected, ",", fixed = TRUE)[[1]] else names(sizes)
scale <- as.numeric(Sys.getenv("SCALE", "1"))
for (name in selected) {
  kernel <- get(name)
  n <- ceiling(sizes[[name]] * scale)
  kernel(max(1, n %/% 20))
  seconds <- system.time(kernel(n))[["elapsed"]]
  cat(name, format(seconds, nsmall = 3), paste(format(checks[[name]]()), collapse = " "), sep = "\t")
  cat("\n")
}
