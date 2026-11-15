# A ball is a List of its four fields, indexed by position as in the R program: x, y, x velocity,
# y velocity.
execute <- function(ball_count: I64) -> F64 {
  seed <- 74755
  next_random <- function(seed: I64) -> I64 Num.bitwise_and(seed * 1309 + 13849, 65535)

  balls <- []
  for (i in List.range(1, ball_count)) {
    seed <- next_random(seed)
    x <- seed %% 500
    seed <- next_random(seed)
    y <- seed %% 500
    seed <- next_random(seed)
    x_vel <- seed %% 300 - 150
    seed <- next_random(seed)
    y_vel <- seed %% 300 - 150
    balls <- List.append(balls, [Num.to_f64(x), Num.to_f64(y), Num.to_f64(x_vel), Num.to_f64(y_vel)])
  }

  bounces <- 0
  for (i in List.range(1, 50)) {
    moved <- List.map(balls, bounce)
    bounces <- bounces + Num.to_f64(List.count_if(moved, \(m) m[[2]]))
    balls <- List.map(moved, \(m) m[[1]])
  }
  bounces
}

bounce <- function(ball: List<F64>) -> Tuple{List<F64>, Bool} {
  x_limit <- 500
  y_limit <- 500
  bounced <- Bool.false

  ball[[1]] <- ball[[1]] + ball[[3]]
  ball[[2]] <- ball[[2]] + ball[[4]]

  if (ball[[1]] > x_limit) {
    ball[[1]] <- x_limit
    ball[[3]] <- 0 - Num.abs(ball[[3]])
    bounced <- Bool.true
  }
  if (ball[[1]] < 0) {
    ball[[1]] <- 0
    ball[[3]] <- Num.abs(ball[[3]])
    bounced <- Bool.true
  }
  if (ball[[2]] > y_limit) {
    ball[[2]] <- y_limit
    ball[[4]] <- 0 - Num.abs(ball[[4]])
    bounced <- Bool.true
  }
  if (ball[[2]] < 0) {
    ball[[2]] <- 0
    ball[[4]] <- Num.abs(ball[[4]])
    bounced <- Bool.true
  }
  (ball, bounced)
}

# ---------------------------------------------------------------------------
# Driver: the same as bounce_nonames.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(10000))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("bounce_nonames %s %.4f\n", checksum, seconds))
})
