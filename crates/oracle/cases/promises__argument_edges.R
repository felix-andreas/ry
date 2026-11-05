# A symbol argument reads the variable when the callee forces it, not when the call is made.
x <- 1
late <- function(a) { x <<- 2; a }
late(x)
x <- 1
via_assign <- function(a) { assign("x", 3, envir = parent.frame()); a }
via_assign(x)
# A forced promise passed on keeps its first value and its expression.
outer <- function(v) { v; inner(v) }
inner <- function(w) list(substitute(w), w)
y <- 5
outer(y)
# Forcing happens once, in the order the callee forces.
noisy <- function(label, value) { cat("forcing", label, "\n"); value }
order_of <- function(p, q) { q; p; p + q }
order_of(noisy("p", 1), noisy("q", 2))
# missing() passes through a wrapper that forwards its own missing formal.
wrap <- function(a) target(a)
target <- function(b) missing(b)
wrap()
wrap(1)
# A promise from delayedAssign is forced once and shared by every reader.
delayedAssign("shared", { cat("evaluating shared\n"); 10 })
reader <- function(s) s * 2
reader(shared)
reader(shared)
# sys.function and match.call inside a callee with dots.
dotted <- function(first, ...) list(match.call(), names(list(...)), sys.function())
r <- dotted(1, b = 2, 3)
r[[1]]
r[[2]]
identical(r[[3]], dotted)
# The callee decides the order, so `q` runs before `p` signals its error.
tryCatch(order_of(stop("first"), noisy("never", 1)), error = function(e) conditionMessage(e))
# A constant argument still reads back through substitute.
quoted <- function(z) substitute(z)
quoted(42L)
