# `x[i, j] <- v` on anything but a matrix names the matrix, three or more subscripts do not,
# and `[[<-` has its own message.
show <- function(expr) {
  out <- tryCatch(expr, error = function(e) {
    paste("error:", conditionMessage(e), "| call:", paste(deparse(conditionCall(e)), collapse = " "))
  })
  print(out)
}
g <- function() 1
cases <- list(
  function() { y <- list(); y[g(), , 1] <- 2; y },
  function() { y <- 1:6; y[g(), , 1] <- 2; y },
  function() { y <- matrix(1:6, 2); y[g(), , 1] <- 2; y },
  function() { y <- matrix(1:6, 2); y[1, 2, 1] <- 2; y },
  function() { y <- list(); y[1, 1] <- 2; y },
  function() { y <- 1:6; y[g(), 1] <- 2; y },
  function() { y <- 1:6; y[1, 1] <- 2; y },
  function() { y <- array(1:8, c(2, 2, 2)); y[g(), 1] <- 2; y },
  function() { y <- array(1:8, c(2, 2, 2)); y[g(), , 1] <- 0L; y },
  function() { y <- NULL; y[g(), 1] <- 2; y },
  function() { y <- list(); y[[g(), 1, 1]] <- 2; y },
  function() { y <- matrix(1:6, 2); y[[g(), 1, 1]] <- 2; y },
  function() { y <- 1:6; y[[1, 1]] <- 2; y },
  function() { y <- NULL; y[[1, 1]] <- 2; y },
  function() { y <- array(1:8, c(2, 2, 2)); y[[1, 1]] <- 2; y },
  function() { y <- matrix(1:6, 2); y[[2, 3]] <- 0L; y },
  function() { y <- 1:6; y[[1, 1]] },
  function() { y <- matrix(1:6, 2); y[[1, 1, 1]] }
)
for (round in 1:3) for (case in cases) show(case())
y <- list()
show({ y[g(), , 1] <- 2; y })
y <- matrix(1:6, 2)
show({ y[1, 2, 1] <- 2; y })
show({ y[[1, 2, 1]] <- 2; y })
