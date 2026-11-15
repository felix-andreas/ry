# boundary: load_scores, report
# A function with an R type in its signature is where R values meet ry values. The interpreter
# runs it, and the native function it calls takes ry values only.
average <- function(scores: List<F64>) -> F64 List.sum(scores) / Num.to_f64(List.len(scores))
load_scores <- function(count: integer) -> List<F64> {
  raw <- sqrt(seq_len(count))
  List.from_r(raw[raw > 1.5])
}
report <- function(count: integer) -> character {
  scores <- load_scores(count)
  sprintf("%.3f", average(scores))
}
cat(report(10L), "\n")
