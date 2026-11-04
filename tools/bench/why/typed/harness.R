# Shared by every benchmark: runs `run()` once cold (GNU R's JIT compiles, ry tiers up), then
# three more times warm, and prints the checksum, the best warm time and the cold time in seconds.
# The clock and the output are R functions, so the harness is where R values cross into ry's.
measure <- function(name: Str, run: fn() -> F64) -> NULL {
  clock <- function() -> F64 Num.from_r(proc.time()[[3]])
  started <- clock()
  value <- run()
  cold <- clock() - started
  best <- Num.infinity_f64
  for (k in List.range(1, 3)) {
    started <- clock()
    again <- run()
    best <- Num.min(best, clock() - started)
    if (again != value) stop("the benchmark is not deterministic")
  }
  cat(sprintf("%s %s %.4f %.4f\n", name, format(value, digits = 15), best, cold))
}
