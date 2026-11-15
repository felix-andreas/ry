# Planned closure calls made from a frame whose code reads its variables by name: after
# `assign`, `eval`, `<<-` on a local's enclosure, a subassignment that may dispatch, or
# `environment()` handed to code that reads it. Each site runs often enough to plan, and the
# callee must see exactly what a call from an ordinary frame shows it: lazy arguments, the
# caller as its parent frame, `sys.call`, `substitute` and `missing`.
add <- function(a, b = 10L) a + b
show_call <- function(x, y) list(sys.call(), substitute(x), missing(y), nargs())
parent_names <- function() sort(ls(parent.frame()))
lazy <- function(a) { assign("v", 100L, envir = parent.frame()); a }
unused <- function(a, b) a
by_assign <- function(n) {
  total <- 0L
  assign("v", 1L)
  for (i in seq_len(n)) {
    v <- i
    total <- total + add(v) + add(v, i) + add(b = 2L, a = v * 2L)
  }
  total
}
print(by_assign(50L))
by_eval <- function(n) {
  out <- NULL
  w <- 3L
  for (i in seq_len(n)) {
    eval(quote(w <- w + 1L))
    out <- show_call(w + i, )
  }
  list(out, w)
}
str(by_eval(20L))
named_frame <- function(n) {
  first <- 1L; second <- "two"
  assign("third", 3)
  seen <- NULL
  for (i in seq_len(n)) seen <- parent_names()
  seen
}
print(named_frame(12L))
# The callee writes the caller's variable before it forces the argument that names it.
late <- function(n) {
  out <- integer(0)
  for (i in seq_len(n)) {
    v <- i
    assign("touched", TRUE)
    out <- c(out, lazy(v))
  }
  out
}
print(late(12L))
# An argument nobody forces runs no code, and one that fails reports the caller's call.
effects <- function(n) {
  count <- 0L
  assign("count", 0L)
  bump <- function() { count <<- count + 1L; count }
  for (i in seq_len(n)) unused(i, bump())
  for (i in seq_len(n)) unused(bump(), i)
  count
}
print(effects(15L))
failing <- function(n) {
  assign("x", 1L)
  results <- character(0)
  for (i in seq_len(n)) {
    results <- c(results, tryCatch(add(stop("no ", i)), error = function(e) conditionMessage(e)))
  }
  results[c(1L, n)]
}
print(failing(12L))
# `return()` in an argument returns from the calling frame, which reads by name.
returns <- function(n) {
  assign("x", 1L)
  for (i in seq_len(n)) {
    if (i == n) add(return("left through an argument"))
    add(i)
  }
  "fell off the end"
}
print(returns(12L))
# An argument that is a function expression, which the argument compiler leaves as source.
apply_one <- function(f, x) f(x)
lambdas <- function(n) {
  assign("offset", 5L)
  total <- 0L
  for (i in seq_len(n)) total <- total + apply_one(function(z) z + offset, i)
  total
}
print(lambdas(30L))
# The frame starts on its slots and switches to reading by name halfway through a loop, so
# one site sees both kinds of caller.
switching <- function(n) {
  total <- 0L
  value <- 1L
  for (i in seq_len(n)) {
    if (i == n %/% 2L) assign("value", 1000L)
    total <- total + add(value, i)
  }
  total
}
print(switching(40L))
# A method called through `UseMethod` runs in a frame with an environment when the generic
# has locals, and its calls are planned like any other.
describe <- function(x, ...) { tag <- "generic"; UseMethod("describe") }
describe.default <- function(x, ...) { total <- 0L; for (i in 1:20) total <- total + add(x, i); total }
print(describe(2L))
# `on.exit` and a handler in the callee, and a callee that keeps its caller's frame.
with_exit <- function(x) { on.exit(cat("exit", x, "\n")); x * 2L }
keeper <- NULL
keep_parent <- function() { keeper <<- parent.frame(); invisible(NULL) }
exits <- function(n) {
  assign("mine", "kept")
  total <- 0L
  for (i in seq_len(n)) { total <- total + with_exit(i); keep_parent() }
  total
}
print(exits(3L))
print(get("mine", envir = keeper))
print(sort(ls(keeper)))
