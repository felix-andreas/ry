# promises that escape the frame
f1 <- function(x) { g <- function(y) function() y; g(x + 1) }
print(f1(1)())
# closure in a list
f2 <- function(a) list(get = function() a, set = function(v) a <<- v)
o <- f2(1); o$set(5); print(o$get())
# environment returned inside a list
f3 <- function(a, b = a * 2) { list(e = environment()) }
e <- f3(3)$e; print(sort(ls(e))); print(e$b)
# lazy default never forced, frame dropped
f4 <- function(a, b = stop("never")) a
print(f4(1))
# default reads a local defined later
f5 <- function(a, b = z) { z <- a * 10; b }
print(f5(2))
# recursion with promises
fib <- function(n) if (n < 2) n else fib(n - 1) + fib(n - 2)
print(fib(15))
# error unwinding through slot frames with environments
f6 <- function(x) { e <- environment(); h6(e) }
h6 <- function(e) stop("inner")
r <- tryCatch(f6(1), error = function(c) conditionMessage(c)); print(r)
# sys.frames / sys.calls
f7 <- function(x) { g7(x) }
g7 <- function(y) { length(sys.frames()) + length(sys.calls()) }
print(f7(1))
# parent.frame assign
f8 <- function() { g8(); v }
g8 <- function() assign("v", 42, envir = parent.frame())
print(f8())
# eval in parent frame reading locals
f9 <- function(a) { b <- a + 1; g9() }
g9 <- function() eval.parent(quote(a + b))
print(f9(1))
# local()
f10 <- function(x) local({ y <- x; y * 2 })
print(f10(4))
# on.exit modifying result via environment
f11 <- function() { x <- 1; on.exit(x <<- 99); x }
print(f11())
# match.arg / missing with defaults
f12 <- function(type = c("a", "b")) { type <- match.arg(type); type }
print(f12()); print(f12("b"))
# do.call / Recall
f13 <- function(n) if (n <= 1) 1 else n * Recall(n - 1)
print(do.call(f13, list(5)))
# sapply closures capturing loop var
f14 <- function(n) { fs <- lapply(seq_len(n), function(i) function() i); sum(vapply(fs, function(f) f(), 1)) }
print(f14(30))
# substitute through two levels and deparse
f15 <- function(x) g15(x)
g15 <- function(y) deparse(substitute(y))
print(f15(a + b))
# missing through promise chain
f16 <- function(x) g16(x)
g16 <- function(y) missing(y)
print(f16())
# environment() as key and reg.finalizer
f17 <- function() { e <- environment(); reg.finalizer(e, function(e) cat("fin\n")); 1 }
print(f17()); invisible(gc())
# returns promise-holding dots
f18 <- function(...) { g <- function(...) ..1; g(...) }
print(f18(7))
f19 <- function(x) { (function(...) nargs())(x, x) }
print(f19(1))
# sys.function and formals
f20 <- function(a, b = 2) names(formals(sys.function()))
print(f20(1))
# nested functions modifying parent via <<-
counter <- function() { i <- 0; function() { i <<- i + 1; i } }
cn <- counter(); cn(); print(cn())
# frame flagged in loop with environment reads
f21 <- function(n) { s <- 0; for (i in 1:n) { e <- environment(); s <- s + get("i", envir = e) }; s }
print(f21(10))
# complex assignment in a partial frame
f22 <- function() { x <- list(a = 1); e <- environment(); x$a <- 5; names(x) <- "b"; e$x$b }
print(f22())
# rm then reassign by name
f23 <- function(x) { e <- environment(); rm("x", envir = e); x <- 3; x + exists("x", envir = e, inherits = FALSE) }
print(f23(1))
# ls ordering after partial boxing
f24 <- function(a, b, c) { z <- 1; y <- 2; environment()$y; ls(sorted = FALSE) }
print(f24(1, 2, 3))
f25 <- function(a, b, c) { z <- 1; y <- 2; e <- environment(); e$c; names(as.list(e, all.names = TRUE)) }
print(f25(1, 2, 3))
# trace of promise with side effect forced once
f26 <- function(x) { g <- function() x; g(); g(); x }
print(f26({cat("once\n"); 1}))
# S4 / R5 quick
setClass("P", representation(v = "numeric"))
setGeneric("val", function(o) standardGeneric("val"))
setMethod("val", "P", function(o) o@v * 2)
print(val(new("P", v = 4)))
# withCallingHandlers + invokeRestart
f27 <- function() withCallingHandlers({ warning("w"); "done" }, warning = function(w) invokeRestart("muffleWarning"))
print(f27())
# browser-less debug helpers
f28 <- function(x) { y <- x; sys.call() }
print(f28(1 + 2))
f29 <- function(x, ...) { mc <- match.call(); as.list(mc)[-1] }
str(f29(1, b = 2))
f30 <- function(a) { nms <- character(); for (nm in c("a")) nms <- c(nms, nm); mget(nms, envir = environment()) }
str(f30(4))
