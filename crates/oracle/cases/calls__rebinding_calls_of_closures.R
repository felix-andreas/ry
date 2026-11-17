# `x <- f(x)` from a function's frame, with a closure `f` and values of every kind: a call
# site that runs often enough to plan must behave as its first call does.
same <- function(x) x
grow <- function(x) c(x, length(x))
first <- function(x) { x[[1L]] <- "first"; x }
named <- function(x, tag = "t") { names(x)[1L] <- tag; x }
rebinding <- function(n) {
  number <- 0L
  text <- "a"
  numbers <- integer(0)
  items <- list("a", 2L)
  grid <- matrix(1:4, 2L)
  frame <- data.frame(a = 1:2)
  nothing <- NULL
  for (i in seq_len(n)) {
    number <- same(number)
    text <- same(text)
    numbers <- grow(numbers)
    items <- first(items)
    items <- named(items, tag = paste0("k", i))
    grid <- same(grid)
    frame <- named(frame)
    nothing <- same(nothing)
  }
  list(number, text, numbers, items, grid, frame, nothing)
}
str(rebinding(8L))

# The callee looks at its call, its caller's frame and its argument's expression.
looks <- function(x) list(sys.call(), substitute(x), sort(ls(parent.frame())), missing(x), nargs())
looking <- function(n) {
  value <- 1L
  for (i in seq_len(n)) value <- looks(value)
  value
}
str(looking(5L))

# The value the callee returns is another variable's, or the callee keeps its argument.
kept <- NULL
keeps <- function(x) { kept <<- x; x + 1L }
other <- function(x) shared
shared <- c(10L, 20L)
aliasing <- function(n) {
  value <- 1L
  for (i in seq_len(n)) {
    value <- keeps(value)
    before <- kept
    pair <- c(1L, 2L)
    pair <- other(pair)
    pair[1L] <- i
  }
  list(value, before, pair, shared)
}
str(aliasing(6L))
print(kept)

# An error or a condition in the callee leaves the variable as it was.
fails <- function(x) if (x > 3L) stop("too large: ", x) else x + 1L
warns <- function(x) { if (x %% 2L == 0L) warning("even: ", x); x + 1L }
failing <- function(n) {
  value <- 0L
  seen <- character(0)
  for (i in seq_len(n)) {
    seen <- c(seen, tryCatch({ value <- fails(value); "ok" }, error = function(e) conditionMessage(e)))
    value <- withCallingHandlers(warns(value), warning = function(w) {
      seen <<- c(seen, conditionMessage(w))
      invokeRestart("muffleWarning")
    })
  }
  list(value, seen)
}
str(failing(6L))

# One site calls a closure, a builtin, a base closure and a closure with `...` in turn, and a
# closure the loop replaces.
turn <- function(n) {
  value <- c(b = 2, a = 1)
  out <- list()
  for (i in seq_len(n)) {
    f <- switch(i %% 5L + 1L, same, rev, unname, function(...) c(...), function(x, y = x) y + i)
    value <- f(value)
    out[[i]] <- value
  }
  out
}
str(turn(12L))

# The target is a variable an inner closure reads, a variable the callee assigns through its
# caller's frame, and an argument of the running function.
captured <- function(n) {
  value <- 1L
  read <- function() value
  out <- integer(0)
  for (i in seq_len(n)) { value <- keeps(value); out <- c(out, read()) }
  out
}
print(captured(6L))
reaches <- function(x) { assign("value", 100L, envir = parent.frame()); x + 1L }
reached <- function(n) {
  value <- 0L
  for (i in seq_len(n)) value <- reaches(value)
  value
}
print(reached(5L))
argument <- function(x, n) { for (i in seq_len(n)) x <- grow(x); x }
start <- c(7L, 8L)
print(argument(start, 5L))
print(start)

# A missing argument, a promise that was never forced, and a default that reads the target.
lazy <- function(x, y = x) if (missing(x)) "missing" else y
defaulted <- function(value, n) {
  for (i in seq_len(n)) value <- lazy(value)
  value
}
print(defaulted(3L, 5L))
print(tryCatch(defaulted(, 5L), error = function(e) conditionMessage(e)))

# Recursion through a rebinding site, and `Recall`.
count_down <- function(x) { if (x > 0L) x <- count_down(x - 1L); x }
print(count_down(40L))
halve <- function(x) { if (length(x) > 1L) x <- Recall(x[seq_len(length(x) %/% 2L)]); x }
print(halve(1:40))
