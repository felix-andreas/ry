# Shared by every benchmark: runs `run()` once cold (GNU R's JIT compiles, ry tiers up), then
# three more times warm, and prints the checksum, the best warm time and the cold time in seconds.
measure <- function(name: character, run: fn() -> double) -> NULL {
  started: double <- proc.time()[[3]]
  value <- run()
  cold: double <- proc.time()[[3]] - started
  best: double <- Inf
  for (k in 1:3) {
    started <- proc.time()[[3]]
    again <- run()
    finished: double <- proc.time()[[3]]
    best <- min(best, finished - started)
    if (!identical(again, value)) stop("the benchmark is not deterministic")
  }
  cat(sprintf("%s %s %.4f %.4f\n", name, format(value, digits = 15), best, cold))
}
