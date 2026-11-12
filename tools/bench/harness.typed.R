# The typed twin of harness.R, prepended to every `<name>.typed.R` program by bench.py, which runs
# it in strict mode. It prints the same line as harness.R, and a typed program returns an `F64`, so
# the checksum is the same 15-digit form harness.R prints for a numeric scalar. The clock, the
# environment and the output are R functions, so `measure` is a boundary function: `name` has an R
# type, which lets the body hold R values and runs it in the interpreter. The program it times, `run`,
# is native.
measure <- function(name: character, run: fn() -> F64) -> NULL {
  clock <- function() -> double proc.time()[[3]]
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
  cat(sprintf("%s %s %.4f %.4f %.4f\n", name, sprintf("%.15g", value),
              stats::median(seconds), min(seconds), cold))
}
