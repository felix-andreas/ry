alias Ball = Record{x: F64, y: F64, x_vel: F64, y_vel: F64}

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
    ball <- (x = Num.to_f64(x), y = Num.to_f64(y), x_vel = Num.to_f64(x_vel), y_vel = Num.to_f64(y_vel))
    balls <- List.append(balls, ball)
  }

  bounces <- 0
  for (i in List.range(1, 50)) {
    moved <- List.map(balls, bounce)
    bounces <- bounces + Num.to_f64(List.count_if(moved, \(m) m$bounced))
    balls <- List.map(moved, \(m) m$ball)
  }
  bounces
}

bounce <- function(ball: Ball) -> Record{ball: Ball, bounced: Bool} {
  x_limit <- 500
  y_limit <- 500
  bounced <- Bool.false

  ball <- (..ball, x = ball$x + ball$x_vel, y = ball$y + ball$y_vel)

  if (ball$x > x_limit) {
    ball <- (..ball, x = x_limit, x_vel = 0 - Num.abs(ball$x_vel))
    bounced <- Bool.true
  }
  if (ball$x < 0) {
    ball <- (..ball, x = 0, x_vel = Num.abs(ball$x_vel))
    bounced <- Bool.true
  }
  if (ball$y > y_limit) {
    ball <- (..ball, y = y_limit, y_vel = 0 - Num.abs(ball$y_vel))
    bounced <- Bool.true
  }
  if (ball$y < 0) {
    ball <- (..ball, y = 0, y_vel = Num.abs(ball$y_vel))
    bounced <- Bool.true
  }
  (ball = ball, bounced = bounced)
}

# ---------------------------------------------------------------------------
# Driver: the same as bounce.R's, except that the R value the checksum reads is
# built from the typed result after the clock stops.
local({
  started <- proc.time()[[3]]
  output <- capture.output(result <- execute(3000))
  seconds <- proc.time()[[3]] - started
  value <- result
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("bounce %s %.4f\n", checksum, seconds))
})
