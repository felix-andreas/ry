# Element-wise chains inside a closure, which compiles them to one fused pass from the second
# call on. The vectors are long enough for the pass to run (it skips fewer than 64 elements). Each runs three times, so the fused pass, its declines and its warnings all show.
three <- function(label, f) {
  for (i in 1:3) value <- withCallingHandlers(f(), warning = function(w) {
    cat(label, "warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning")
  })
  cat(label, typeof(value), "\n"); print(value)
}
x <- rep(c(1.5, -2, NA, NaN, 0, 3), 11)
y <- rep(c(NaN, NA, 1, 2, -1, 4), 11)
i <- rep(c(1L, -2L, NA, 4L, 5L, 2147483647L), 11)
j <- rep(c(2L, 3L, 4L, NA, 1L, 0L), 11)
l <- rep(c(TRUE, FALSE, NA, TRUE, TRUE, FALSE), 11)
xs <- rep(c(1.5, -2, 0, 3, 4, -5) * 1.1, 11)
is <- rep(c(1L, -2L, 7L, 4L, 5L, 9L), 11)

# NA and NaN keep the first operand's payload through every node.
three("chain", function() x * y + x)
three("chain2", function() y * x + y)
three("payload", function() (x + y) - (y + x))
three("power", function() x^2 + y^2)
three("negate", function() -x * y)
three("mixed", function() is * 1.5 + xs / 3 - 2L)

# Integer chains: overflow gives NA with one warning per evaluation.
three("int", function() is * 2L + j)
three("overflow", function() i + 1L - j)
three("intdiv", function() i / j + 1L)
three("negint", function() -i + j)
three("abs", function() abs(i) + 1L)

# Comparisons and logic.
three("compare", function() x * 2 > y)
three("logic", function() (x > 0) & (y > 0) | l)
three("not", function() !(x > 0) & l)
three("logical", function() l + l * 2L)

# Reductions over a fused tree.
three("sum_na", function() sum(x > 0))
three("sum_int_na", function() sum(i - j))
three("sum_gt", function() sum(xs > 0))
three("sum_double", function() sum(xs * 2 + 1))
three("sum_int", function() sum(is * 2L - 1L))
three("sum_not", function() sum(!(xs > 0)))
three("sum_wide", function() sum(rep(c(2147483647L, 1L), 32) * 1L + 0L))
three("any", function() any(xs * 2 > 7))
three("any_na", function() any(x + 1 > 2))
three("any_overflow", function() any(i + 1L > 0L))
three("all", function() all(is + 1L > -5L))
three("all_false", function() all(xs - 1 < 3))
three("all_na", function() all(y * 0 == 0))

# Math functions, with the "NaNs produced" warning.
gbm <- function(mu, sigma, dt, shock) exp((mu - sigma^2 / 2) * dt + sigma * sqrt(dt) * shock)
three("gbm", function() gbm(0.05, 0.2, 1 / 252, xs))
three("gbm_na", function() gbm(0.05, 0.2, 1 / 252, rep(c(-1, 0, 1, NA, NaN, 2), 11)))
three("sqrt_nan", function() sqrt(x - 1) + 1)
three("math", function() sqrt(abs(xs)) * 2 + exp(-xs))
three("floor_int", function() floor(i + 1L) * 2)
three("trig", function() sin(xs) * cos(xs) + tan(xs / 10))
three("long", function() {
  z <- seq(-1, 1, length.out = 3001)
  c(sum(z > 0), sum(z * z), any(z > 0.999), all(z >= -1), sum(exp(-z * z / 2) * 0.001))
})

# Recycling, attributes and empty operands.
three("recycle", function() c(1, 2, 3) * c(1, 2) + 1)
three("recycle_int", function() 1:6 * c(1, 2) + 1)
m <- matrix(1:6, 2)
three("matrix", function() m * 2 + 1)
named <- c(a = 1, b = 2)
three("names", function() named * 2 + 1)
three("empty", function() numeric(0) * 2 + 1)

# Operands keep their values: the result never reuses a variable's storage.
v <- c(1, 2, 3)
keep <- function(v) { w <- v * 2 + 1; list(v, w) }
three("keep", function() keep(v))
# A promise forced in the middle of the chain sees the operands read before it.
g <- function(a, b) a * b + a
three("promise", function() g(x, { x[1] <- 100; y }))

# Rebinding a math function after compiling takes effect at once. (GNU R's byte compiler
# inlines the arithmetic operators without checking, so rebinding those differs by design.)
q <- function() exp(xs * 2 + 1)
for (n in 1:3) q()
exp <- function(a) "mine"
print(q())
rm(exp)
print(q())
h <- function(z) { exp <- function(q) "local"; exp(z * 2 + 1) }
three("local_function", function() h(xs))

# A tree that may warn scans every element, and a settled `any` or `all` keeps its answer.
settles <- function(a, b) c(any(NA | (abs(a) != sin(b))), all(NA & (abs(a) == sin(b))))
for (i in 1:4) print(settles(5, c(0.25, -4, 9, 1, 16, 100)))
