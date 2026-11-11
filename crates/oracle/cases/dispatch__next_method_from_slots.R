# `NextMethod()` from a method that runs in slots, without an environment: the next method's
# arguments are promises over the method's frame, so they see what the method assigned to its
# formals, `substitute` shows the formal's name, and a promise the next method keeps past both
# returns still reads the method's value. As in GNU R, the next method's parent frame is the
# method's own caller, not the method's frame.
g <- function(x, ...) UseMethod("g")
g.default <- function(x, ...) paste("default", x, ...)
g.a <- function(x, ...) paste("a", x, deparse(substitute(x)), ...)
g.b <- function(x, ...) { x <- x * 10; paste("b", NextMethod()) }
g.c <- function(x, ...) paste("c", NextMethod())
g.lazy <- function(x, ...) NextMethod()
g.keep <- function(x, ...) { x <- "kept"; NextMethod() }
g.default2 <- function(x, ...) paste("default2", NextMethod())

later <- function(x, ...) UseMethod("later")
later.keep <- function(x, ...) { x <- "kept"; NextMethod() }
later.default <- function(x, ...) function() x

peek <- function(x) UseMethod("peek")
peek.a <- function(x) { local <- "seen"; NextMethod() }
peek.default <- function(x) exists("local", envir = parent.frame(), inherits = FALSE)

poke <- function(x) UseMethod("poke")
poke.a <- function(x) { NextMethod(); exists("planted", inherits = FALSE) }
poke.default <- function(x) assign("planted", TRUE, envir = parent.frame())

named <- function(x, y = 1, ...) UseMethod("named")
named.a <- function(x, y = 1, ...) paste("named.a", y, ...)
named.b <- function(x, y = 1, ...) NextMethod(y = 3)
named.c <- function(x, y = 1, ...) NextMethod(y = y + 100, "extra")

miss <- function(x, y) UseMethod("miss")
miss.a <- function(x, y) if (missing(y)) "y missing" else paste("y", y)
miss.b <- function(x, y) NextMethod()

calls <- function(x, ...) UseMethod("calls")
calls.a <- function(x, ...) paste(deparse(sys.call()), deparse(match.call()), nargs())
calls.b <- function(x, ...) NextMethod()

vars <- function(x) UseMethod("vars")
vars.a <- function(x) paste(.Generic, paste(.Class, collapse = "/"), class(parent.frame()))
vars.b <- function(x) NextMethod()

direct <- function(x) UseMethod("direct")
direct.a <- function(x) "direct.a"
direct.b <- function(x) paste("direct.b", NextMethod())

steps <- character()
note <- function(tag, value) { steps <<- c(steps, tag); value }
forced <- function(x, y) UseMethod("forced")
forced.a <- function(x, y) { steps <<- c(steps, "a body"); paste(y, x) }
forced.b <- function(x, y) { steps <<- c(steps, "b body"); NextMethod() }

`[.sl` <- function(x, i) { r <- NextMethod(); class(r) <- class(x); r }
Ops.sl <- function(e1, e2) { r <- NextMethod(); class(r) <- "sl"; r }

for (i in 1:60) {
  r <- c(
    g(structure(i, class = c("b", "a"))),
    g(structure(i, class = c("c", "b", "a"))),
    g(structure(i, class = c("lazy", "a"))),
    g(structure(i, class = c("keep", "a"))),
    g(structure(i, class = c("default2"))),
    later(structure(i, class = "keep"))(),
    peek(structure(i, class = "a")),
    poke(structure(i, class = "a")),
    named(structure(i, class = c("b", "a")), 5),
    named(structure(i, class = c("c", "a")), 5),
    named(structure(i, class = c("c", "a"))),
    miss(structure(i, class = c("b", "a"))),
    miss(structure(i, class = c("b", "a")), 2),
    calls(structure(i, class = c("b", "a")), 7),
    vars(structure(i, class = c("b", "a"))),
    unclass(structure(c(i, 2, 3), class = "sl")[2]),
    unclass(structure(c(i, 2), class = "sl") + 1)
  )
  if (i == 1 || i == 60) print(r)
}
steps <- character()
print(forced(structure(1, class = c("b", "a")), note("y forced", "y")))
print(steps)
print(tryCatch(direct.b(1), error = conditionMessage))
f <- function(x) UseMethod("f")
f.a <- function(x) { x <- 2; NextMethod() }
f.default <- function(x) x
print(sapply(1:3, function(k) f(structure(k, class = "a"))))
# The next method's parent frame is the generic's caller, through every level of a chain, and
# the next method is looked up from that caller, which can define it locally.
p <- function(x) UseMethod("p")
p.a <- function(x) { marker_a <- 1; NextMethod() }
p.b <- function(x) { marker_b <- 1; NextMethod() }
p.default <- function(x) {
  e <- parent.frame()
  c(caller = exists("marker_f", envir = e, inherits = FALSE),
    a = exists("marker_a", envir = e, inherits = FALSE),
    b = exists("marker_b", envir = e, inherits = FALSE),
    global = identical(e, globalenv()))
}
calls_p <- function() { marker_f <- 1; p(structure(1, class = c("b", "a"))) }
print(calls_p())
print(p(structure(1, class = c("b", "a"))))
shadow <- function(v) { g.a <- function(x, ...) "local g.a"; g(structure(v, class = c("b", "a"))) }
not_function <- function(v) { g.a <- 1; g(structure(v, class = c("b", "a"))) }
for (i in 1:3) print(c(shadow(i), not_function(i)))
# A next method that never forces an argument lets the method return without its frame; one
# that keeps the argument's promise, in a closure, a list of promises or a condition, still
# reads the method's value after both return.
unused <- function(x, y) UseMethod("unused")
unused.a <- function(x, y) "unused.a"
unused.b <- function(x, y) { y <- "late"; NextMethod() }
held <- function(x, y) UseMethod("held")
held.a <- function(x, y) list(function() y, environment())
held.b <- function(x, y) { y <- "kept by a"; NextMethod() }
caught <- function(x, y) UseMethod("caught")
caught.a <- function(x, y) structure(class = "condition", list(message = "kept", get = function() y))
caught.b <- function(x, y) { y <- "kept by a condition"; NextMethod() }
for (i in 1:40) {
  r <- c(unused(structure(i, class = c("b", "a")), stop("never forced")),
    held(structure(i, class = c("b", "a")), i)[[1]](),
    get("y", envir = held(structure(i, class = c("b", "a")), i)[[2]]),
    caught(structure(i, class = c("b", "a")), i)$get())
  if (i == 1 || i == 40) print(r)
}
