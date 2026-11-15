# A call site that has planned which argument fills which slot binds the arguments straight
# into the callee's slots. Each function runs many times so its body compiles and its call
# sites plan, and every result must match a call that matched its arguments afresh.

# Left-out formals with literal defaults at the start, middle and end, around supplied ones,
# so binding reaches each default in turn.
literals <- function(a = 1L, b, c = "c", d = NULL, e = TRUE, f) {
  list(a, if (missing(b)) "no b" else b, c, d, e, if (missing(f)) "no f" else f)
}
calls_literals <- function(x) {
  list(literals(b = x), literals(x, f = x), literals(), literals(d = x, e = FALSE))
}
for (i in 1:30) r <- calls_literals(i)
str(r)

# A default that reads a later formal, and one that reads a local assigned before it is forced.
later <- function(x, n = length(y), y = x * 2) {
  z <- 10
  m <- function(k = z + n) k
  z <- 20
  c(n, y, m())
}
local_default <- function(x, total = sum(x) + offset) {
  offset <- 100
  total
}
for (i in 1:30) r <- list(later(1:3), later(1:3, y = 1), local_default(1:4), local_default(1, 0))
str(r)

# missing() through a chain of promises: a left-out formal passed on is missing in the
# callee, but a formal with a default is not missing once the callee receives it.
inner_missing <- function(p, q = 1) c(missing(p), missing(q))
middle_missing <- function(p, q) inner_missing(p, q)
outer_missing <- function(p, q = 2) middle_missing(p, q)
for (i in 1:30) {
  r <- list(outer_missing(), outer_missing(1), outer_missing(q = 3), middle_missing())
}
str(r)

# Forwarding `...` plans per call, since what `...` holds changes between calls.
collect <- function(a, ..., z = "z") list(a = a, dots = list(...), z = z)
forward <- function(...) collect(...)
forward_named <- function(x, ...) collect(x, ..., z = "named")
for (i in 1:30) {
  r <- list(forward(1), forward(1, 2, 3), forward(z = 0, 1, b = 2), forward_named(i, w = 1))
}
str(r)

# The frame of a planned call gets an environment in the middle of the call, while defaults
# are still unforced, and they still read the frame's values afterwards.
reify_midway <- function(x, scale = 2, shift = x * scale) {
  frames <- length(sys.frames())
  env <- environment()
  assign("x", x + 1, envir = env)
  c(frames > 0, shift, get("scale", envir = env))
}
for (i in 1:30) r <- list(reify_midway(1), reify_midway(1, 3), reify_midway(1, shift = 0))
str(r)

# What the call stack reports from a planned call.
who <- function(a, b = 2) {
  list(sys.call(), deparse(sys.function())[1], identical(parent.frame(), globalenv()), b)
}
who_inside <- function(v) who(v)
for (i in 1:30) r <- list(who(1), who_inside(2))
str(r)

# on.exit runs in order, after the value is computed, for a planned call that returns early
# and for one that signals an error.
log <- character()
exits <- function(stop_early, fail = FALSE) {
  on.exit(log <<- c(log, "first"))
  on.exit(log <<- c(log, "second"), add = TRUE)
  if (fail) stop("failed")
  if (stop_early) return("early")
  "late"
}
for (i in 1:30) {
  log <- character()
  r <- list(exits(TRUE), exits(FALSE), tryCatch(exits(FALSE, TRUE), error = conditionMessage))
}
str(list(r, log))

# match.arg and match.call in a planned callee.
pick <- function(type = c("linear", "quadratic", "cubic"), degree) {
  type <- match.arg(type)
  list(type, match.call())
}
for (i in 1:30) r <- list(pick(), pick("quad"), pick(degree = 3, "cubic"))
str(r)

# A closure whose environment is replaced keeps its formals and body, so the site's plan
# holds, and the body reads its free variables from the new environment.
scaled <- function(x, by = 1) x * by * factor
environment(scaled) <- list2env(list(factor = 2))
use_scaled <- function(x) scaled(x)
for (i in 1:30) {
  if (i == 15) environment(scaled) <- list2env(list(factor = 10))
  r <- use_scaled(i)
}
str(r)

# A closure whose formals are replaced keeps its body, so a site that planned for the old
# formals plans again: the defaults and their positions changed.
reshaped <- function(a, b = 1) a + b
call_reshaped <- function() reshaped(1)
for (i in 1:30) {
  if (i == 15) formals(reshaped) <- alist(a = , b = a * 100)
  r <- call_reshaped()
}
str(r)

# One site calls closures with other formals in turn, and one argument's value comes from an
# active binding that calls the same site with yet another closure while the arguments are
# collected.
apply_one <- function(f, v) f(v)
makeActiveBinding("reentrant", function() apply_one(function(w, extra = 5) w + extra, 1), globalenv())
pass_active <- function(f) f(reentrant)
for (i in 1:30) {
  r <- list(
    apply_one(function(x, y = 2) x + y, i),
    apply_one(function(x) -x, i),
    pass_active(function(p, q = "q") list(p, q))
  )
}
str(r)

# Calls nested past the recursion limit fail, and planned calls work afterwards.
deep <- function(n) if (n == 0) 0 else 1 + deep(n - 1)
old <- options(expressions = 500)
r <- tryCatch(deep(10000), error = function(e) "too deep")
options(old)
str(list(r, deep(10)))
