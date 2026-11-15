# `UseMethod` dispatches only as the innermost call of the generic, on the argument as passed
# (not the variable reassigned since), and NextMethod does the same.
g <- function(x) { r <- try(UseMethod("g"), silent = TRUE); cat("after try:", class(r), conditionMessage(attr(r, "condition")), "\n"); "end" }
g.default <- function(x) { cat("g.default\n"); "method" }
print(g(1))
f <- function(x) { x <- structure(1, class = "k"); UseMethod("f") }
f.k <- function(x) "f.k"
f.default <- function(x) "f.default"
print(f(c(10, 20)))
h <- function(x) { x <- structure(1, class = "k"); r <- try(UseMethod("h"), silent = TRUE); UseMethod("h") }
h.k <- function(x) stop("h.k")
h.default <- function(x) x
w <- function() { w <- c(30, 40); h(w) }
print(w())
k2 <- function(x) local(UseMethod("k2"))
k2.default <- function(x) "k2.default"
print(tryCatch(k2(3), error = conditionMessage))
k3 <- function(x) (UseMethod("k3"))
k3.default <- function(x) "k3.default"
print(k3(3))
k4 <- function(x) { if (TRUE) { UseMethod("k4") } }
k4.default <- function(x) "k4.default"
print(k4(3))
print(tryCatch(UseMethod("zz"), error = conditionMessage))
k5 <- function(x) { x <- 2; UseMethod("k5") }
k5.default <- function(x) "k5.default"
k5.numeric <- function(x) "k5.numeric"
k5.NULL <- function(x) "k5.NULL"
print(k5())
k6 <- function(x, y) { x <- "a"; UseMethod("k6") }
k6.character <- function(x, y) "k6.character"
k6.default <- function(x, y) c("k6.default", class(y))
print(k6(y = factor("q")))
k7 <- function(x) { x <- structure(1, class = "k"); UseMethod("k7", x) }
k7.k <- function(x) c("k7.k", x)
k7.default <- function(x) "k7.default"
print(k7(5))
k8 <- function(...) { UseMethod("k8") }
k8.k <- function(...) "k8.k"
k8.default <- function(...) "k8.default"
print(k8(structure(1, class = "k")))
k9 <- function(x) { force(x); x <- structure(1, class = "k"); UseMethod("k9") }
k9.k <- function(x) "k9.k"
k9.default <- function(x) c("k9.default", x)
print(k9(7))
k10 <- function(x) { x <- structure(1, class = "k"); identity(UseMethod("k10")) }
k10.default <- function(x) "k10.default"
print(tryCatch(k10(1), error = conditionMessage))
k11 <- function(x) { x <- structure(1, class = "k"); suppressWarnings(UseMethod("k11")) }
k11.default <- function(x) "k11.default"
print(tryCatch(k11(1), error = conditionMessage))
k12 <- function(x) { x <- structure(1, class = "k"); withCallingHandlers(UseMethod("k12"), warning = identity) }
k12.default <- function(x) "k12.default"
print(tryCatch(k12(1), error = conditionMessage))
k13 <- function(x) UseMethod("k13")
k13.k <- function(x) { x <- 0; r1 <- NextMethod(); r2 <- NextMethod(); c(r1, r2) }
k13.default <- function(x) unclass(x) + 1
print(k13(structure(5, class = "k")))
k14 <- function(x) { x <- 0; UseMethod("k14", structure(1, class = "k")) }
k14.k <- function(x) { x <- 0; r1 <- NextMethod(); r2 <- NextMethod(); c(r1, r2) }
k14.default <- function(x) x + 1
print(k14(5))
k15 <- function(x, ...) UseMethod("k15")
k15.default <- function(x, ...) "k15.default"
k15.k <- function(x, ...) "k15.k"
print(k15(1, x = structure(1, class = "k")))
print(k15(xx = 1, structure(1, class = "k")))
print(tryCatch(k15(x = 1, x = 2), error = conditionMessage))
a <- function(x) UseMethod("a")
a.k <- function(x) { r <- try(NextMethod(), silent = TRUE); c(class(r), "after") }
a.default <- function(x) "a.default"
print(a(structure(1, class = "k")))
b <- function(x) UseMethod("b")
b.k <- function(x) identity(NextMethod())
b.default <- function(x) "b.default"
print(b(structure(1, class = "k")))
