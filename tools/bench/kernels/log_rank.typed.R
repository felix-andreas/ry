# A hot path from the biostatistics code base: the log-rank statistic, a loop over event times
# with whole-list comparisons in its body.
log_rank <- function(time: List<I64>, event: List<Bool>, treated: List<Bool>) -> F64 {
  count <- function(mask: List<Bool>) -> F64 Num.to_f64(List.count_if(mask, function(b) b))
  times <- List.sort_asc(Set.to_list(Set.from_list(time[event])))
  observed <- 0
  expected <- 0
  variance <- 0
  for (t in times) {
    risk <- time >= t
    died <- time == t & event
    n <- count(risk)
    n1 <- count(risk & treated)
    d <- count(died)
    d1 <- count(died & treated)
    observed <- observed + d1
    expected <- expected + d * n1 / n
    if (n > 1) variance <- variance + d * (n1 / n) * (1 - n1 / n) * (n - d) / (n - 1)
  }
  (observed - expected) * (observed - expected) / variance
}
subjects <- List.range(1, 16000)
time <- List.map(subjects, function(i) (i * 7919) %% 1000 + 1)
event <- List.map(subjects, function(i) i %% 3 != 0)
treated <- List.map(subjects, function(i) i %% 2 == 0)
measure("log_rank", function() log_rank(time, event, treated))
