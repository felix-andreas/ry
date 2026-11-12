# The typed twin of harness.R, prepended to every `<name>.typed.R` program by bench.py, which runs
# it in strict mode. It prints the same line as harness.R, and a typed program returns an `F64`, so
# the checksum is the same 15-digit form harness.R prints for a numeric scalar. The clock, the
# environment and the output are R functions, so the harness is where R values cross into ry's.
# The clock is `Sys.time()` and the times print to the microsecond, where harness.R reads the
# millisecond clock of `proc.time()`: a typed program can finish a run in under a millisecond, and
# the typed suite compares it with a Rust program that does.
measure <- function(name: Str, run: fn() -> F64) -> NULL {
  clock <- function() -> F64 Num.from_r(as.numeric(Sys.time()))
  repeats <- Num.from_r(as.integer(Sys.getenv("RY_BENCH_REPEAT", "5")))
  started <- clock()
  value <- run()
  cold <- clock() - started
  times <- List.with_capacity(repeats)
  for (k in List.range(1, repeats)) {
    started <- clock()
    again <- run()
    times <- List.append(times, clock() - started)
    if (again != value) stop("the program is not deterministic")
  }
  seconds <- List.to_r(times)
  cat(sprintf("%s %s %.6f %.6f %.6f\n", name, sprintf("%.15g", value),
              stats::median(seconds), min(seconds), cold))
}
