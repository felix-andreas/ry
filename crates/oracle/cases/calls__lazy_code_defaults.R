# A formal left out of a call whose default is code binds lazily: the default runs on first
# use, in the callee's frame, exactly as a promise of it would. Each function runs many times
# so its body compiles and its call sites plan.

# substitute() on a defaulted formal returns the default expression, forced or not.
subst <- function(x = a + b * 2, force = FALSE) {
  a <- 1
  b <- 2
  if (force) x
  substitute(x)
}
for (i in 1:30) r <- list(subst(), subst(force = TRUE), subst(q))
str(r)

# A default that reads itself, directly or through another formal, is an error at the force.
self_ref <- function(x = x) x
mutual <- function(x = y, y = x) x
for (i in 1:30) {
  r <- list(
    tryCatch(self_ref(), error = function(e) conditionMessage(e)),
    tryCatch(mutual(), error = function(e) conditionMessage(e)),
    self_ref(1)
  )
}
str(r)

# The default reads a local assigned after entry but before its first force, and a local
# reassigned after it was forced keeps the forced value.
timing <- function(x = n * 10) {
  n <- 1
  n <- n + 1
  first <- x
  n <- 100
  c(first, x)
}
for (i in 1:30) r <- timing()
print(r)

# A default first forced inside on.exit sees the frame's locals as they are at exit.
wrap_exit <- function() {
  out <- NULL
  f <- function(x = paste("exit", k)) {
    k <- "late"
    on.exit(out <<- x)
    "body"
  }
  list(f(), out)
}
for (i in 1:30) r <- wrap_exit()
str(r)

# A default forced from a frame walk: sys.function() and parent.frame() of a callee, and a
# variable read through the caller's environment.
walked <- function(x = length(y), y = 1:5) {
  peek()
}
peek <- function() {
  frame <- parent.frame()
  c(eval(quote(x), frame), length(formals(sys.function(-1))))
}
for (i in 1:30) r <- walked()
print(r)

# A default forced after the frame gets its environment mid-call, and one captured by a
# closure that outlives the frame.
reified <- function(x = y + 1, y = 10) {
  env <- environment()
  assign("y", 20, envir = env)
  x
}
captured <- function(x = y * 2, y = 3) {
  function() x
}
for (i in 1:30) {
  r <- list(reified(), captured()(), captured(y = 5)())
}
str(r)

# A defaulted formal passed on through `...` and as a named argument, forced in the callee.
dots_on <- function(...) list(...)
pass_on <- function(x = y + 1, y = 1) c(dots_on(x)[[1]], identity(x))
for (i in 1:30) r <- pass_on()
print(r)

# missing() on a defaulted formal is TRUE before and after the default is forced.
missing_default <- function(x = 5) {
  before <- missing(x)
  value <- x
  c(before, missing(x), value)
}
for (i in 1:30) r <- list(missing_default(), missing_default(7))
str(r)

# A default that makes the frame observable once forced: it calls environment() and
# sys.call() of the frame it belongs to.
observing <- function(x = environment(), y = sys.call()) {
  list(is.environment(x), y)
}
for (i in 1:30) r <- observing()
str(r)

# delayedAssign in the body interleaves with a lazy default.
delayed <- function(x = log("x forced")) {
  log <- function(m) {
    trace <<- c(trace, m)
    m
  }
  delayedAssign("z", log("z forced"))
  z
  x
}
trace <- character()
for (i in 1:30) {
  trace <- character()
  r <- delayed()
}
print(trace)
print(r)

# A default whose force fails leaves it unforced, so the next read runs it again.
retry <- function(x = { count <<- count + 1; if (count < 2) stop("first try") else count }) {
  first <- tryCatch(x, error = function(e) conditionMessage(e))
  list(first, x)
}
for (i in 1:30) {
  count <- 0
  r <- retry()
}
str(r)
