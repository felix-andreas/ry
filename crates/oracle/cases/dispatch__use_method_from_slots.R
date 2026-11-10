# A generic whose body is `UseMethod(...)` dispatches from its slot frame without an
# environment: the dispatch object comes from the first formal (or the first supplied argument
# when it is missing), the method gets the generic's promises and call, and its parent frame is
# the generic's caller.
g <- function(x, ...) UseMethod("g")
g.default <- function(x, ...) paste("default", deparse(sys.call()), length(list(...)))
g.a <- function(x, ...) {
  caller <- parent.frame()
  paste("a", exists("marker", envir = caller, inherits = FALSE), x, ...)
}
g.b <- function(x, ...) paste("b", NextMethod())
h <- function(x = 1, y) UseMethod("h")
h.default <- function(x = 1, y) paste("h default", missing(x), if (missing(y)) "-" else y)
h.character <- function(x = 1, y) paste("h character", y)
d <- function(...) UseMethod("d")
d.numeric <- function(...) paste("d numeric", ...)
l <- function(x) { local <- "seen"; UseMethod("l") }
l.default <- function(x) if (exists("local", inherits = FALSE)) local else "hidden"
n <- function(x) { if (is.numeric(x)) UseMethod("n"); "fell through" }
n.default <- function(x) "n default"
k <- function(x) UseMethod("k")
k.default <- function(x) "k default"
k.a <- function(x) paste("k.a", identical(.GenericCallEnv, parent.frame()), NextMethod())
k.b <- function(x) {
  e <- .GenericCallEnv
  paste("k.b", exists("token", envir = e, inherits = FALSE), get("token", envir = e))
}
calls_k <- function(v, cls) { token <- v; k(structure(v, class = cls)) }
shadow <- function(v) { k.a <- function(x) "local k.a"; k(structure(v, class = "a")) }
len <- function(x) UseMethod("len")
len.default <- length
calls_len <- function(v) { n <- 1; len(v) + n }
side <- 0
lazy <- function(v) { side <<- side + 1; v }
caller <- function(v) { marker <- TRUE; g(structure(v, class = "a"), "extra") }
for (i in 1:60) {
  r <- c(
    caller(i %% 3),
    g(structure(2, class = c("b", "a")), "z"),
    g(lazy(5), 1, 2),
    h(y = "only y"),
    h("s", y = "named"),
    d(4, 5),
    l(1),
    n(1),
    n("x"),
    calls_k(7, "a"),
    calls_k(8, "b"),
    shadow(1),
    calls_len(1:4)
  )
}
cat(r, side, sep = "\n")
