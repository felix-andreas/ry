# Times data.table's file reading and writing and the functions data.table code calls beside `[`,
# and prints one tab-separated line per run: `question  run  seconds  rows  cols  checksum`, the
# same lines `cargo bench -p table --bench ops_benchmark` prints for the ry engine. Every
# question runs twice, and an input a question modifies in place is copied outside the clock.
#
# Usage: Rscript ops.R <data-dir>, where the directory holds `groupby.csv` from datagen.R.

args <- commandArgs(trailingOnly = TRUE)
dir <- args[[1]]
suppressMessages(library(data.table, lib.loc = Sys.getenv("RY_BENCH_RLIB", "~/R/library")))
setDTthreads(0L)
cat(sprintf("# data.table %s, %d threads\n", packageVersion("data.table"), getDTthreads()))

checksum <- function(answer) {
  if (is.data.frame(answer)) {
    numeric <- Filter(is.numeric, answer)
    return(sum(vapply(numeric, function(column) sum(as.numeric(column), na.rm = TRUE), numeric(1))))
  }
  if (is.list(answer)) return(length(answer))
  if (is.character(answer)) return(0)
  sum(as.numeric(answer), na.rm = TRUE)
}

report <- function(question, run, seconds, answer) {
  rows <- if (is.data.frame(answer)) nrow(answer) else if (is.list(answer)) length(answer[[1]]) else length(answer)
  cols <- if (is.data.frame(answer)) ncol(answer) else if (is.list(answer)) length(answer) else 1L
  cat(sprintf("%s\t%d\t%.4f\t%d\t%d\t%.6e\n", question, run, seconds, rows, cols, checksum(answer)))
}

# `prepare` runs before the clock starts on each run, so an in-place question gets a fresh copy.
time_question <- function(question, expr, prepare = NULL) {
  expr <- substitute(expr)
  prepare <- substitute(prepare)
  for (run in 1:2) {
    eval(prepare, parent.frame())
    gc()
    seconds <- system.time(answer <- eval(expr, parent.frame()))[["elapsed"]]
    report(question, run, seconds, answer)
    rm(answer)
  }
}

path <- file.path(dir, "groupby.csv")
out <- file.path(dir, "ops_written.csv")
time_question("fread", x <- fread(path, showProgress = FALSE))
time_question("fwrite", { fwrite(x, out); x })
time_question("setkey", setkey(y, id1, id4), y <- copy(x))
time_question("setorder", setorder(y, id3, -v3), y <- copy(x))
time_question("setnames", setnames(y, c("id1", "v1"), c("first", "value")), y <- copy(x))
time_question("setcolorder", setcolorder(y, c("v3", "v2")), y <- copy(x))
y <- NULL
time_question("unique", unique(x, by = c("id1", "id2")))
time_question("uniqueN", uniqueN(x, by = c("id1", "id2", "id4")))
time_question("rbindlist", rbindlist(list(x, x)))
time_question("melt", melt(x, id.vars = c("id1", "id2", "id3"), measure.vars = c("v1", "v2", "v3")))
time_question("dcast", dcast(x, id1 ~ id4, value.var = "v1", fun.aggregate = sum))
time_question("shift", shift(x$v1))
test <- x$v1 > 2L
time_question("fifelse", fifelse(test, x$v3, -x$v3))
gappy <- fifelse(test, x$v3, NA_real_)
fallback <- as.numeric(x$v2)
time_question("fcoalesce", fcoalesce(gappy, fallback))
time_question("nafill", nafill(gappy, "locf"))
time_question("rleid", rleid(x$id4))
time_question("between", between(x$v3, 25, 75))
time_question("chin", x$id1 %chin% c("id001", "id002", "id003"))
time_question("tstrsplit", tstrsplit(x$id3, "d", fixed = TRUE))
unlink(out)
