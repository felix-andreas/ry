# Kernels from a user's profile of a large package's test suite (about 415 test files under
# `ry load`): each is one code shape that the profile ranked high, cut down to a loop. K1 to K5 are
# the user's code as received. The report reached us cut off after K5, so K6, K7 and K8 are
# reconstructions from the report's one-line descriptions of them, and so are the sizes and the
# runner. `set.seed` and the `checks` list are ours: they make the data and one result per kernel
# comparable between GNU R and ry.
#
# Usage: [KERNELS=k1,k3] [SCALE=1] Rscript kernels.R
# Prints one line per kernel, `kernel  seconds  checksum`: the time of one run after a warm-up run
# of a twentieth the size, and a checksum of one call's result. The sizes put GNU R near the
# report's times, and the warm-up share makes K1 create the 21,000 objects the report counted.

suppressPackageStartupMessages({ library(methods); library(data.table); library(xts) })
set.seed(20240102)

setClass("Key", slots = c(a = "character", b = "character", c = "character",
                          d = "character", e = "integer", f = "integer"))
invisible(setValidity("Key", function(object) {
  fields <- list(a = object@a, b = object@b, c = object@c, d = object@d, e = object@e, f = object@f)
  if (length(unique(purrr::map_int(fields, length))) != 1) "slots must be same length" else TRUE
}))
makeKey <- function(a = NA_character_, b = NA_character_, c = NA_character_,
                    d = NA_character_, e = NA_integer_, f = NA_integer_)
  new("Key", a = a, b = b, c = c, d = d, e = e, f = f)
keyString <- function(k, useB = TRUE, useC = TRUE, useD = FALSE, useE = FALSE, useF = TRUE) {
  args <- list(k@a, if (useB) k@b, if (useC) k@c, if (useD) k@d, if (useE) k@e, if (useF) k@f,
               sep = "|") |> purrr::compact()
  paste |> do.call(args)
}
k1 <- function(n) for (i in seq_len(n)) keyString(makeKey(a = "AB12", b = "src", c = "close"))

asDate <- function(x, ...) zoo::as.Date(x, ...)
k2 <- function(n) { d <- as.Date("2024-01-02"); for (i in seq_len(n)) { asDate("2024-03-15"); asDate(d) } }

isMissing <- function(x) {
  if (isS4(x)) return(FALSE)
  if (data.table::is.data.table(x)) return(ifelse(length(x) > 1, all(is.na(x)), TRUE))
  if (class(x)[1] == "character")
    return(ifelse(length(x) > 1, all(is.na(x) | (x == "NA")), !isTRUE(!(is.na(unname(x)) || (x == "NA")))))
  ifelse(length(x) > 1, all(is.na(x)), !isTRUE(!is.na(unname(x))))
}
k3 <- function(n) for (i in seq_len(n)) { isMissing("abc"); isMissing(NA_integer_); isMissing(3.5) }

argNames <- function(...) vapply(as.list(substitute(list(...)))[-1], deparse, "")
checkTypes <- function(objects, types, objectNames) {
  for (i in seq_along(objects)) {
    o <- objects[[i]]
    if (!identical(class(o)[1], types[[i]]) && !is(o, types[[i]])) stop("bad type ", objectNames[i])
  }
  invisible(TRUE)
}
checkArgs <- function(..., types = NULL, env = NULL) {
  objects <- list(...)
  if (is.null(types)) return(invisible(TRUE))
  delayedAssign("objectNames", argNames(...))
  checkTypes(objects, types, objectNames)
  if (is.null(env)) env <- parent.frame()
  invisible(TRUE)
}
k4 <- function(n) { x <- "a"; y <- 1L; z <- Sys.Date()
  for (i in seq_len(n)) checkArgs(x, y, z, types = c("character", "numeric", "Date")) }

setClass("Store", slots = c(s1 = "list", s2 = "list", s3 = "list"))
idx <- as.Date("2020-01-01") + 0:499
series <- function(p) setNames(lapply(1:20, function(j) xts(rnorm(500), idx)), paste0(p, 1:20))
store <- new("Store", s1 = series("A"), s2 = series("B"), s3 = series("C"))
inStore <- function(object, key) {
  slots <- lapply(slotNames(object), function(s) slot(object, s))
  inSlots <- as.character(unlist(lapply(slots, names), use.names = FALSE))
  paste(key, "close", sep = "|") %in% paste(inSlots, "close", sep = "|")
}
k5 <- function(n) for (i in seq_len(n)) inStore(store, c("A3", "B7", "Z1"))

xl <- lapply(1:8, function(j) xts(rnorm(250), sort(sample(idx, 250))))

# Reconstructed: "do.call(xts::merge.xts, c(list_of_8_xts, all = TRUE, fill = zoo::na.locf))".
mergeAll <- function(series) do.call(xts::merge.xts, c(series, all = TRUE, fill = zoo::na.locf))
k6 <- function(n) for (i in seq_len(n)) mergeAll(xl)

# Reconstructed: "acc <- rbind(acc, data.table(...), fill = TRUE) in a loop".
collect <- function(rows) {
  acc <- data.table()
  for (j in seq_len(rows))
    acc <- rbind(acc, data.table(id = j, name = paste0("n", j), value = j * 1.5), fill = TRUE)
  acc
}
k7 <- function(n) for (i in seq_len(n)) collect(10L)

# Reconstructed: "small matrix arithmetic on column slices with pmin and pmax".
rates <- matrix(rnorm(60, 0.02, 0.01), nrow = 12, dimnames = list(NULL, paste0("c", 1:5)))
clip <- function(m, floor = 0, cap = 0.03) {
  low <- m[, 1:2, drop = FALSE]
  high <- m[, 3:4, drop = FALSE]
  pmax(pmin(low * 1.1 + high, cap), floor) - pmin(low, high) / (1 + pmax(abs(high), m[, 5]))
}
k8 <- function(n) for (i in seq_len(n)) clip(rates)

sizes <- c(k1 = 20000, k2 = 30000, k3 = 100000, k4 = 60000, k5 = 20000, k6 = 4000, k7 = 500, k8 = 30000)
checks <- list(
  k1 = function() keyString(makeKey(a = "AB12", b = "src", c = "close")),
  k2 = function() format(c(asDate("2024-03-15"), asDate(as.Date("2024-01-02")))),
  k3 = function() c(isMissing("abc"), isMissing(NA_integer_), isMissing(3.5), isMissing("NA")),
  k4 = function() { x <- "a"; y <- 1L; z <- Sys.Date()
    checkArgs(x, y, z, types = c("character", "numeric", "Date")) },
  k5 = function() inStore(store, c("A3", "B7", "Z1")),
  k6 = function() { merged <- mergeAll(xl); c(dim(merged), round(sum(merged, na.rm = TRUE), 6)) },
  k7 = function() { acc <- collect(10L); c(dim(acc), sum(acc$value), nchar(paste(acc$name, collapse = ""))) },
  k8 = function() round(sum(clip(rates)), 10)
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
