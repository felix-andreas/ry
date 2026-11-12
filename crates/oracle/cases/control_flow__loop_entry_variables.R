# Loops whose variables are loaded before the loop, entered again partway through after compiled
# code gave up on an iteration. Each entry loads those variables from the frame and must decline
# when one no longer has the type the loop was compiled for, is a binding that runs code, or is
# a promise nothing has forced.

widens <- function(n) {
  x <- 1L
  s <- 0
  for (i in seq_len(n)) {
    if (i == n %/% 2L) x <- 2.5
    s <- s + x
  }
  c(s, x)
}
print(widens(10L))
print(widens(3000L))

shrinks <- function(n) {
  x <- 2.5
  s <- 0
  for (i in seq_len(n)) {
    if (i == n %/% 3L) x <- "a"
    if (is.character(x)) s <- s + 1 else s <- s + x
  }
  c(s, nchar(x))
}
print(shrinks(3000L))

reads <- 0L
makeActiveBinding("counted", function() {
  reads <<- reads + 1L
  2
}, globalenv())
active <- function(n) {
  s <- counted
  for (i in seq_len(n)) s <- s + counted
  s
}
print(active(2000L))
print(reads)

lazy <- function(a, n) {
  for (i in seq_len(n)) if (i > 1000L) return(a)
  "never forced"
}
print(lazy(stop("forced too early"), 500L))
print(lazy("forced late", 2000L))

zero <- function(a) {
  for (i in seq_len(0)) a
  "zero iterations"
}
print(zero(stop("forced by a loop that never ran")))

missing_inside <- function(n) {
  for (i in seq_len(n)) undefined_variable
  "no iteration read it"
}
print(missing_inside(0L))
print(tryCatch(missing_inside(1L), error = conditionMessage))

nested <- function(a, b) {
  a <- as.double(a)
  b <- as.double(b)
  out <- double(length(a) + length(b))
  for (i in seq_along(a)) {
    for (j in seq_along(b)) out[i + j] <- out[i + j] + a[i] * b[j]
    if (i == 7L) b <- b * 2L
  }
  sum(out)
}
print(nested(1:20, 1:30))
print(nested(c(1.5, 2), 1:3))
