# The typed twin of harness.R, prepended to every `<name>.typed.R` program by bench.py, which runs
# it in strict mode. It prints the same line as harness.R, and a typed program returns an `F64`, so
# the checksum is the same 15-digit form harness.R prints for a numeric scalar. The clock, the
# environment and the output are R functions, so `measure` is a boundary function: `name` has an R
# type, which lets the body hold R values and runs it in the interpreter. The program it times, `run`,
# is native. The clock is `Sys.time()` and the times print to the microsecond, where harness.R reads
# the millisecond clock of `proc.time()`: a typed program can finish a run in under a millisecond,
# and the typed suite compares it with a Rust program that does.
measure <- function(name: character, run: fn() -> F64) -> NULL {
  clock <- function() -> double as.numeric(Sys.time())
  repeats <- as.integer(Sys.getenv("RY_BENCH_REPEAT", "5"))
  started <- clock()
  value <- run()
  cold <- clock() - started
  seconds <- numeric(repeats)
  for (k in seq_len(repeats)) {
    started <- clock()
    again <- run()
    seconds[[k]] <- clock() - started
    if (again != value) stop("the program is not deterministic")
  }
  cat(sprintf("%s %s %.6f %.6f %.6f\n", name, sprintf("%.15g", value),
              stats::median(seconds), min(seconds), cold))
}
