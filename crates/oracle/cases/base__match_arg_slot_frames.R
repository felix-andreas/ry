# match.arg called from functions that run often enough to keep their frames in slots, so the
# fast path reads the caller's default without the caller's environment.
pick <- function(type = c("linear", "quadratic", "cubic")) match.arg(type)
one <- function(mode = "only") match.arg(mode)
for (i in 1:60) { a <- pick(); b <- pick("quad"); d <- one() }
print(c(a, b, d))
print(tryCatch(pick("q2"), error = conditionMessage))
print(tryCatch(pick(c("linear", "cubic")), error = conditionMessage))
# Defaults made of several character vectors, and choices passed explicitly.
multi <- function(x = c(c("a", "b"), "c")) match.arg(x)
two <- function(x = c("aa", "bb"), y = "bb") match.arg(y, c("aa", "bb"))
for (i in 1:60) { m <- multi("c"); t <- two() }
print(c(m, t))
# A `c` the caller binds locally, as a formal, or in its enclosure is not base's, so the fast
# path declines and the R body evaluates the default with that `c`.
local_c <- function(kind = c("x", "y")) { c <- function(...) "z"; match.arg(kind) }
formal_c <- function(c = 1, kind = c("x", "y")) match.arg(kind)
maker <- function() { c <- function(...) "w"; function(kind = c("x", "y")) match.arg(kind) }
enclosed_c <- maker()
for (i in 1:60) { l <- local_c(); f <- formal_c(kind = "y"); e <- enclosed_c() }
print(c(l, f, e))
# A default that is not a constant.
built <- function(kind = paste0(c("p", "q"), "1")) match.arg(kind)
for (i in 1:60) k <- built("q1")
print(k)
# several.ok, through the fast path's decline.
many <- function(kind = c("red", "green", "blue")) match.arg(kind, several.ok = TRUE)
for (i in 1:60) s <- many(c("re", "bl"))
print(s)
# Callers whose frame already has an environment.
seen <- function(kind = c("north", "south")) { environment(); match.arg(kind) }
who <- function(kind = c("east", "west")) { sys.function(); match.arg(kind) }
for (i in 1:60) { n <- seen("so"); w <- who() }
print(c(n, w))
# Forwarded through `...`, and called with the argument from a caller's variable.
inner <- function(kind = c("min", "max"), ...) match.arg(kind)
outer <- function(...) inner(...)
for (i in 1:60) { o <- outer(); v <- "ma"; p <- outer(kind = v) }
print(c(o, p))
