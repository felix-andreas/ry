# bench(label, f) times f(i). It doubles the number of calls until one batch takes 0.3 s of CPU
# time, then prints the label and the nanoseconds per call of the last batch. With the environment
# variable UNDER_HANDLER=1, the calls run inside withCallingHandlers(), as the code of a test does.
under_handler <- nzchar(Sys.getenv("UNDER_HANDLER"))
run <- function(n, f) for (i in seq_len(n)) f(i)
bench <- function(label, f) {
  n <- 64
  repeat {
    start <- sum(proc.time()[1:2])
    if (under_handler) withCallingHandlers(run(n, f), warning = function(w) NULL) else run(n, f)
    seconds <- sum(proc.time()[1:2]) - start
    if (seconds >= 0.3) break
    n <- n * 2
  }
  cat(sprintf("%s\t%.0f\n", label, 1e9 * seconds / n))
}

# Pattern: distribution functions and matrix arithmetic on medium vectors, as in a simulation.
p <- seq(0.001, 0.999, length.out = 10000)
m <- matrix(rnorm(250 * 20), ncol = 20)
w <- rep(1 / 20, 20)

bench("math: qt() of 10,000 probabilities, df = 5", function(i) qt(p, df = 5))
bench("math: qnorm() of 10,000 probabilities", function(i) qnorm(p))
bench("math: pnorm() of 10,000 values", function(i) pnorm(p))
bench("math: pt() of 10,000 values, df = 5", function(i) pt(p, df = 5))
bench("math: exp(), log() and sqrt() of 10,000 values", function(i) sqrt(exp(log(p))))
bench("math: cumprod(1 + x) of 10,000 values", function(i) cumprod(1 + p / 1000))
bench("math: matrix %*% vector, 250 x 20", function(i) m %*% w)
bench("math: cov() of a 250 x 20 matrix", function(i) cov(m))
bench("math: colMeans() and apply(m, 2, sd)", function(i) colMeans(m) + apply(m, 2, sd))
bench("math: quantile() of 10,000 values", function(i) quantile(p, c(0.01, 0.05)))
bench("math: sort() of 10,000 values", function(i) sort(p, decreasing = TRUE))
bench("math: pmin() and pmax() of 10,000 values", function(i) pmax(pmin(p, 0.9), 0.1))
bench("math: scalar loop with 1,000 steps", function(i) { s <- 0; for (j in 1:1000) s <- s + j * 0.5; s })
