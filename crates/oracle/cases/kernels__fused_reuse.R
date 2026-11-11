# A fused chain assigned to `y` may write into the vector `y` holds only when nothing else can
# see that vector. Each block runs inside a function (slot frames) and at top level or through
# an environment, three times, so the JIT's tiers see it too.
x <- seq(0.5, 30, length.out = 300)

# Another binding shares the old value: `z` keeps it.
slot_alias <- function(x) { y <- x + 0; z <- y; y <- x * 2 + 1; c(sum(z), sum(y)) }
env_alias <- function(x) {
  e <- new.env()
  assign("y", x + 0, envir = e)
  local({ z <- y; y <- x * 2 + 1; c(sum(z), sum(y)) }, envir = e)
}
shared_arg <- function(x) { y <- x; x <- x * 2 + 1; c(sum(x), sum(y)) }
for (i in 1:3) print(c(slot_alias(x), env_alias(x), shared_arg(x)))
y <- x + 0; z <- y; y <- x * 2 + 1; print(c(sum(z), sum(y)))

# The result carries the tree's attributes, never those of the vector it overwrites.
attrs <- function(x) {
  y <- structure(x + 0, names = paste0("n", seq_along(x)), dim = c(10L, 30L), class = "foo")
  y <- x * 2 + 1
  list(attributes(y), class(y), sum(y))
}
named <- function(x) { y <- x + 0; y <- setNames(x, paste0("a", seq_along(x))) * 2 + 1; head(y, 3) }
for (i in 1:3) { str(attrs(x)); print(named(x)) }
m <- matrix(x, 10)
dimmed <- function(m) { y <- m + 0; y <- m * 2 + 1; dim(y) }
for (i in 1:3) print(dimmed(m))

# The tree reads `y` itself, once or several times.
self <- function(x) { y <- x + 0; for (i in 1:3) y <- y * 2 + y; sum(y) }
self_math <- function(x) { y <- x + 0; for (i in 1:3) y <- sqrt(y) * 0.5 + exp(-y); sum(y) }
for (i in 1:3) print(c(self(x), self_math(x)))

# `y` has another length or type than the result.
longer <- function(x) { y <- numeric(1000); y <- x * 2 + 1; length(y) }
shorter <- function(x) { y <- numeric(10); y <- x * 2 + 1; length(y) }
typed <- function(x) { y <- seq_len(300); y <- x * 2 + 1; typeof(y) }
ints <- function(n) { y <- numeric(300); y <- seq_len(300) * n + 1L; typeof(y) }
for (i in 1:3) print(list(longer(x), shorter(x), typed(x), ints(3L)))

# A promise or a closure captured the old value before the assignment.
promised <- function(x) {
  y <- x + 0
  p <- (function(v) function() sum(v))(y)
  y <- x * 2 + 1
  c(p(), sum(y))
}
lazy <- function(x) {
  y <- x + 0
  f <- function(v) { y <<- x * 2 + 1; sum(v) }
  c(f(y), sum(y))
}
closure <- function(x) {
  y <- x + 0
  g <- function() sum(y)
  before <- g()
  y <- x * 2 + 1
  c(before, g(), sum(y))
}
delayed <- function(x) {
  y <- x + 0
  delayedAssign("d", y)
  y <- x * 2 + 1
  c(sum(d), sum(y))
}
for (i in 1:3) print(c(promised(x), lazy(x), closure(x), delayed(x)))
