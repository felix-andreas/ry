# Times the base R kernels that the benchmark dependency set leans on, and prints one
# tab-separated line per run: `question  run  seconds  rows  cols  checksum`, the format of
# `tools/bench/table/ops.R`. Every question runs twice on data built from a fixed seed, so GNU R
# and ry see the same inputs and must give the same checksums.
#
# Usage: Rscript kernels.R <rows> [question], where `question` is a regular expression that picks
# the questions to run. With a question given, the last line is `#peak <kB>`, the process's peak
# resident set size, which `tools/bench/base/run.py --memory` compares.

args <- commandArgs(trailingOnly = TRUE)
n <- as.integer(as.numeric(args[[1]]))
only <- if (length(args) >= 2) args[[2]] else NULL
library_path <- Sys.getenv("RY_BENCH_RLIB", "~/R/library")
suppressMessages({
  library(zoo, lib.loc = library_path)
  library(xts, lib.loc = library_path)
})

checksum <- function(answer) {
  if (is.data.frame(answer)) {
    numeric <- Filter(is.numeric, answer)
    text <- Filter(function(column) is.character(column) || is.factor(column), answer)
    return(sum(vapply(numeric, function(column) sum(as.numeric(column), na.rm = TRUE), numeric(1))) +
      sum(vapply(text, function(column) sum(nchar(as.character(column)), na.rm = TRUE), numeric(1))))
  }
  if (is.list(answer)) return(length(answer) + sum(lengths(answer)))
  if (is.character(answer)) return(sum(nchar(answer), na.rm = TRUE))
  if (is.factor(answer)) return(sum(as.integer(answer), na.rm = TRUE))
  sum(as.numeric(answer), na.rm = TRUE)
}

report <- function(question, run, seconds, answer) {
  rows <- if (is.list(answer) && !is.data.frame(answer)) length(answer) else NROW(answer)
  cols <- if (is.list(answer) && !is.data.frame(answer)) 1L else NCOL(answer)
  cat(sprintf("%s\t%d\t%.4f\t%d\t%d\t%.6e\n", question, run, seconds, rows, cols, checksum(answer)))
}

time_question <- function(question, expr) {
  if (!is.null(only) && !grepl(only, question)) return(invisible())
  expr <- substitute(expr)
  for (run in 1:2) {
    gc()
    seconds <- system.time(answer <- eval(expr, parent.frame()))[["elapsed"]]
    report(question, run, seconds, answer)
  }
}

set.seed(42)
groups <- max(10L, n %/% 1000L)
x <- round(runif(n, 0, 100), 4)
i <- sample.int(n)
g <- sample.int(groups, n, replace = TRUE)
h <- sample.int(7L, n, replace = TRUE)
s <- sprintf("id%07d_%s", sample.int(groups * 10L, n, replace = TRUE), sample(letters, n, replace = TRUE))
keys <- unique(s)[seq_len(min(1000L, length(unique(s))))]
df <- data.frame(g = g, h = h, x = x)
lookup <- data.frame(g = seq_len(groups), weight = round(runif(groups), 4))
m <- matrix(x, ncol = 10L)
rows <- n %/% 10L
z <- zoo(x[seq_len(rows)], as.Date("2000-01-01") + seq_len(rows))
xt <- as.xts(z)
cat(sprintf("# %s, %d rows\n", R.version.string, n))

time_question("sort dbl", sort(x))
time_question("order dbl", order(x))
time_question("sort int", sort(i))
time_question("sort chr", sort(s))
time_question("order chr", order(s))
time_question("order two keys", order(g, -x))
time_question("paste0", paste0("k", i, "_", h))
time_question("paste collapse", paste(s, collapse = ","))
time_question("sprintf", sprintf("%05d-%.2f", i, x))
time_question("format", format(x, nsmall = 2))
time_question("formatC", formatC(x, width = 10, format = "f", digits = 3))
time_question("as.character dbl", as.character(x))
time_question("grepl", grepl("_[aeiou]$", s))
time_question("grepl fixed", grepl("00", s, fixed = TRUE))
time_question("sub", sub("^id0*", "", s))
time_question("gsub", gsub("[0-9]", "#", s))
time_question("gsub perl", gsub("(\\d)(\\d)", "\\2\\1", s, perl = TRUE))
time_question("regmatches", regmatches(s, regexpr("[1-9][0-9]*", s)))
time_question("strsplit", strsplit(s, "_", fixed = TRUE))
time_question("nchar", nchar(s))
time_question("substr", substr(s, 3L, 9L))
time_question("toupper", toupper(s))
time_question("split", split(x, g))
time_question("unsplit", unsplit(split(x, g), g))
time_question("tapply", tapply(x, g, sum))
time_question("aggregate", aggregate(x ~ g, data = df, FUN = mean))
time_question("ave", ave(x, g))
time_question("merge", merge(df, lookup, by = "g"))
time_question("match chr", match(s, keys))
time_question("in chr", s %in% keys)
time_question("match int", match(g, seq_len(groups) * 2L))
time_question("unique chr", unique(s))
time_question("duplicated dbl", duplicated(x))
time_question("rowSums", rowSums(m))
time_question("colSums", colSums(m))
time_question("rowMeans", rowMeans(m))
time_question("colMeans", colMeans(m))
time_question("apply rows", apply(m[seq_len(min(nrow(m), 1e5)), ], 1L, max))
time_question("cumsum", cumsum(x))
time_question("cummax", cummax(x))
time_question("diff", diff(x, lag = 3L))
time_question("rle", rle(sort(h)))
time_question("table one", table(g))
time_question("table two", table(g %% 100L, h))
time_question("cut", cut(x, breaks = 0:20 * 5))
time_question("findInterval", findInterval(x, 0:100))
time_question("rollmean", rollmean(z, 20L))
time_question("rollapply", rollapply(z[seq_len(min(rows, 1e5))], 20L, mean))
time_question("xts lag", lag.xts(xt, 5L))
time_question("xts merge", merge(xt, lag.xts(xt, 1L)))
time_question("xts period", period.apply(xt, endpoints(xt, "months"), mean))
time_question("zoo na.locf", na.locf(replace(z, seq(1L, rows, by = 7L), NA)))

if (!is.null(only)) {
  status <- readLines("/proc/self/status")
  cat(sprintf("#peak %s\n", sub("VmHWM:\\s*(\\d+) kB", "\\1", grep("^VmHWM", status, value = TRUE))))
}
