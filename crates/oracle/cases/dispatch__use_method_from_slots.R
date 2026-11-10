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

# An explicit dispatch object, and a lazy argument that defines a function.
as_thing <- function(x, ...) UseMethod("as_thing", x)
as_thing.default <- function(x, ...) paste("default", x)
as_thing.character <- function(x, ...) paste("chr", x, ...)
wrap <- function(v) as_thing(v, "!")
print(wrap("a"))
print(wrap(1))
explicit <- function(x, y) UseMethod("as_thing", y)
print(explicit(1, "b"))
`%or%` <- function(a, b) if (is.null(a)) b else a
lazy_fn <- function(x) x %or% tryCatch(stop("no"), error = function(e) conditionMessage(e))
print(lazy_fn(NULL))
print(lazy_fn(3))

# Superassignment from a frame without an environment.
counter <- function() {
  count <- 0
  handled <- FALSE
  bump <- function(by) {
    count <<- count + by
    handled <<- TRUE
    seen <<- by
  }
  bump(2)
  bump(3)
  c(count, handled, seen)
}
print(counter())
print(exists("seen"))
set_global <- function(v) fresh_global <<- v
print(withVisible(set_global(7))$visible)
print(fresh_global)

# switch() over the caller's `...` from a frame without an environment.
kind_of <- function(x) if (is.numeric(x)) "num" else if (is.character(x)) "chr" else "other"
switch_dots <- function(.x, ...) switch(kind_of(.x), ...)
print(switch_dots(1, num = "a number", chr = "a string", "something else"))
print(switch_dots("s", num = "a number", chr = , str = "a string"))
print(switch_dots(TRUE, num = "a number", "default"))
print(withVisible(switch_dots(TRUE, num = 1))$visible)
by_position <- function(i, ...) switch(i, ...)
print(by_position(2, "one", "two", "three"))
lazy_arm <- function(.x, ...) switch(kind_of(.x), ...)
print(lazy_arm(1, num = { cat("forced num\n"); 1 }, chr = { cat("forced chr\n"); 2 }))
print(tryCatch(switch_dots(1, chr = "x", "a", "b"), error = conditionMessage))
