# A hot path from the biostatistics code base: the log-rank statistic, a loop over event times
# with whole-vector comparisons in its body.
log_rank <- function(data: list{time: integer[], event: integer[], arm: character[]}) -> double {
  times <- sort(unique(data$time[data$event == 1L]))
  observed <- 0
  expected <- 0
  variance <- 0
  for (t in times) {
    risk <- data$time >= t
    n <- sum(risk)
    n1 <- sum(risk & data$arm == "treated")
    d <- sum(data$time == t & data$event == 1L)
    d1 <- sum(data$time == t & data$event == 1L & data$arm == "treated")
    observed <- observed + d1
    expected <- expected + d * n1 / n
    if (n > 1) variance <- variance + d * (n1 / n) * (1 - n1 / n) * (n - d) / (n - 1)
  }
  (observed - expected)^2 / variance
}
subjects <- 4000L
data <- list(
  time = (seq_len(subjects) * 7919L) %% 1000L + 1L,
  event = as.integer(seq_len(subjects) %% 3L != 0L),
  arm = ifelse(seq_len(subjects) %% 2L == 0L, "treated", "control")
)
measure("log_rank", function() log_rank(data))
