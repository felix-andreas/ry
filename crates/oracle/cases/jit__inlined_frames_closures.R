# A closure captures the environment of the call that creates it. A call the optimizing tier
# runs without a frame has none, so it must get its frame before it creates a closure, on a
# branch that had not run when the caller was compiled and on one that then runs every time.

leaf <- function(x, k) {
  if (k < 0) stop("negative")
  if (k == 1) return((function(y) missing(y))())
  x + k
}
for (i in 1:30) leaf(1, 0)
root <- function(p, k) {
  t <- p + 1
  leaf(t - 1, k)
}
for (i in 1:60) root(1, 0)
print(root(3, 1))
print(root(3, 0))

maker <- function(x, k) {
  if (k > 1) return(function() x * k)
  x - k
}
for (i in 1:30) maker(1, 0)
caller <- function(p, k) {
  t <- p * 2
  made <- maker(t, k)
  if (is.function(made)) made() else made
}
for (i in 1:60) caller(1, 0)
print(caller(3, 2))
print(caller(1, 0))
for (i in 1:40) caller(3, 2)
print(caller(3, 4))
print(caller(1, 0))

mapper <- function(x, k) {
  if (k > 1) return(local({ y <- x; y * k }))
  if (k < -1) return(sapply(1:3, function(i) i + x))
  x - k
}
for (i in 1:30) mapper(1, 0)
outer_call <- function(p, k) {
  t <- p * 2
  mapper(t, k)
}
for (i in 1:60) outer_call(1, 0)
print(outer_call(1, 2))
print(outer_call(1, -2))
print(outer_call(1, 0))

counting <- function(x, k) {
  if (k > 1) {
    total <- 0
    add <- function(value) total <<- total + value
    add(x)
    add(k)
    return(total)
  }
  x - k
}
for (i in 1:30) counting(1, 0)
tally <- function(p, k) {
  t <- p * 2
  counting(t, k)
}
for (i in 1:60) tally(1, 0)
print(tally(5, 2))
print(tally(5, 0))
