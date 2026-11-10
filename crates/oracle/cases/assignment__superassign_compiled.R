# `symbol <<- value` compiles to a tier-0 instruction: the nearest enclosing binding is updated,
# the global environment gets a new binding when none exists, active bindings run, locked
# bindings refuse, the value is invisible, and a local binding of the same name is untouched.
counter <- function() {
  count <- 0
  function() { count <<- count + 1; count }
}
next_count <- counter(); for (i in 1:5) next_count(); print(next_count())
outer <- function() {
  x <- 0
  middle <- function() { inner <- function() { for (i in 1:10) x <<- x + i }; inner() }
  middle(); x
}
print(outer())
make_global <- function() { fresh_superassign_global <<- 42 }
make_global(); print(fresh_superassign_global)
shadow <- function() {
  y <- "outer"
  f <- function() { y <- "local"; y <<- "changed"; y }
  c(f(), y)
}
print(shadow())
visible <- function() { z <- 1; g <- function() z <<- 2; withVisible(g()) }
str(visible())
e <- new.env(); local({ hits <- 0 }, envir = e)
makeActiveBinding("watched", function(v) if (!missing(v)) cat("set to", v, "\n") else 0, e)
setter <- local(function() for (i in 1:3) watched <<- i * 10, envir = new.env(parent = e))
setter()
locked <- function() {
  k <- 1
  lockBinding("k", environment())
  f <- function() k <<- 2
  tryCatch(f(), error = function(err) conditionMessage(err))
}
print(locked())
super_while <- function(n) {
  x <- 0
  f <- function() while (x < n) x <<- x + 1
  f(); x
}
print(super_while(1000))
# Replacement forms of `<<-` update the enclosing binding through the replacement function.
replace_forms <- function() {
  x <- list(a = 0, b = 2); names_of <- c(p = 1, q = 2); m <- matrix(0, 2, 2)
  inner <- function() {
    for (i in 1:2) x$a <<- x$a + i
    names(names_of)[2] <<- "b"
    for (j in 1:2) m[, j] <<- j
    attr(x, "note") <<- "set"
  }
  inner()
  list(x, names_of, m)
}
str(replace_forms())
missing_target <- function() { f <- function() nowhere_superassign_target$a <<- 1; tryCatch(f(), error = function(e) conditionMessage(e)) }
print(missing_target())
# An index that assigns another variable leaves the target in place; one that assigns the
# target itself still sees the old value.
grow <- function() {
  freqs <- integer(2); cap <- 2L; n <- 0L
  for (key in c("a", "b", "a", "c", "d", "c")) {
    if (is.null(attr(freqs, key))) {
      n <- n + 1L
      freqs[[cap <- if (cap < n) 2L * cap else cap]] <- 0L
      attr(freqs, key) <- 0L
    }
    attr(freqs, key) <- attr(freqs, key) + 1L
  }
  list(cap = cap, freqs = freqs)
}
str(grow())
same <- function() { x <- 1:3; x[{x[2] <- 30L; 1}] <- 10L; x }
print(same())
other <- function() { x <- 1:3; y <- 0L; x[{y <- 2L; y}] <- 20L; list(x, y) }
str(other())
via_assign <- function() { x <- 1:3; x[{assign("x", 0L); 1}] <- 10L; x }
print(via_assign())
via_assign_part <- function() { x <- 1:3; x[{x[[2]] <- 30L; 1}] <- 10L; x }
print(via_assign_part())
via_super <- function() {
  x <- 1:3
  inner <- function() { x[{x[2] <<- 30L; 1}] <<- 10L; x }
  list(inner(), x)
}
str(via_super())
via_call <- function() {
  x <- 1:3
  poke <- function() { x[2] <<- 30L; 1 }
  x[poke()] <- 10L
  x
}
print(via_call())
via_string <- function() { x <- c(1, 2); x[{"x" <- c(7, 8); 2}] <- 0; x }
print(via_string())
