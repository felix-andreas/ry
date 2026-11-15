# Compiled code runs a closure call without a frame when the call leaves formals out or names
# its arguments. A literal default is the formal's value, a default that is code is never
# evaluated ahead of a read, and the frame the call gets when it needs one after all binds every
# formal as argument matching would have: the reflective reads in the rare branches below see
# the call, the missing formals and the unevaluated defaults as GNU R shows them.

scale <- function(x, by = 2, shift = 0L, label = NULL, extra = stop("never read")) {
  if (x < 0) {
    return(list(match.call(), missing(by), missing(shift), missing(label), sys.call(), nargs()))
  }
  if (is.null(label)) x * by + shift else paste(label, x * by + shift)
}
for (i in 1:30) scale(1)
plain <- function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + scale(i)
  total
}
named <- function(n, last) {
  total <- 0
  for (i in seq_len(n)) total <- total + scale(by = 3, x = i)
  list(total, scale(shift = 1L, x = last, by = 4))
}
for (i in 1:40) plain(100)
for (i in 1:40) named(100, 5)
print(plain(10))
print(named(10, 5))
print(named(10, -5))
print(scale(-1))

derived <- function(x, twice = x * 2, unset, verbose = FALSE) {
  if (verbose) {
    return(list(missing(twice), missing(unset), substitute(twice), twice, match.call()))
  }
  if (x > 1000) unset
  x + twice
}
for (i in 1:30) derived(1)
through <- function(n, verbose) {
  total <- 0
  for (i in seq_len(n)) total <- total + derived(x = i)
  list(total, derived(n, verbose = verbose), derived(verbose = verbose, x = n, twice = n + 1))
}
for (i in 1:40) through(100, FALSE)
print(through(10, FALSE))
print(through(10, TRUE))
print(tryCatch(derived(2000), error = function(e) conditionMessage(e)))
over <- function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + derived(i + 995)
  total
}
print(tryCatch(over(3), error = function(e) conditionMessage(e)))
for (i in 1:40) tryCatch(over(3), error = function(e) NULL)
print(tryCatch(over(8), error = function(e) conditionMessage(e)))

# A default that reads another formal sees the value that formal has when the default is
# evaluated, which is after the callee assigned to it.
late <- function(x, y = x + 1) {
  x <- x * 10
  y
}
for (i in 1:30) late(1)
late_root <- function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + late(i)
  total
}
for (i in 1:40) late_root(100)
print(late_root(4))

# An empty argument leaves its formal missing, and so does a formal handed on while missing.
gap <- function(a, b = 5, c = 7) {
  if (missing(b)) a + c else a + b + c
}
for (i in 1:30) gap(1, , 2)
gaps <- function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + gap(i, , 2) + gap(i, 1) + gap(c = 3, i)
  total
}
for (i in 1:40) gaps(100)
print(gaps(5))

# A formal the callee assigns is a local from then on. The frame the call gets later holds the
# assigned value in place of the argument's promise, and the formal is no longer missing.
fill <- function(x, what = NULL, sep) {
  if (is.null(what)) what <- x * 2L
  if (x > 50L) x <- 50L
  if (x == 13L) return(list(missing(what), substitute(what), substitute(x), missing(sep), match.call()))
  x + what
}
for (i in 1:30) fill(i)
filled <- function(n) {
  total <- 0L
  for (i in seq_len(n)) {
    r <- fill(i)
    if (is.list(r)) return(list(r, total))
    total <- total + r + fill(i, 1L)
  }
  total
}
for (i in 1:40) filled(12L)
print(filled(12L))
print(filled(20L))

# A formal that is a number on one path and another object on the other is read from the frame
# again after the paths join, so its promise and its missing state stay as the call left them.
branch <- function(x, c, d = 3L) {
  t <- x + 1L
  u <- d + 1L
  if (c) {
    x <- "a"
    d <- "b"
  }
  list(substitute(x), missing(d), substitute(d), t, u)
}
branches <- function(i, c) branch(i * 2L, c)
for (k in 1:40) branches(k, FALSE)
str(branches(5L, FALSE))
str(branches(5L, TRUE))
for (k in 1:40) branch(k, FALSE)
str(branch(5L, FALSE))
str(branch(5L + 1L, TRUE))
