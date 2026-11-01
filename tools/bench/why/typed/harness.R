# Shared by every benchmark: runs `run()` once to warm up (GNU R's JIT compiles, ry tiers up),
# then three timed times, and prints the checksum and the best time in seconds.
measure <- function(name: character, run: fn() -> double) -> NULL {
  value <- run()
  best: double <- Inf
  for (k in 1:3) {
    started: double <- proc.time()[[3]]
    again <- run()
    finished: double <- proc.time()[[3]]
    best <- min(best, finished - started)
    if (!identical(again, value)) stop("the benchmark is not deterministic")
  }
  cat(sprintf("%s %s %.4f\n", name, format(value, digits = 15), best))
}
