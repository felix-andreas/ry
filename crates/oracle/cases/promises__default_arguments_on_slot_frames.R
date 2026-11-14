f1 <- function(x, y = x * 2, z = y) { if (missing(y)) cat("y missing\n"); x + y + z }
for (i in 1:4) print(f1(i))
f2 <- function(a, b = a) g2(b)
g2 <- function(v) c(missing(v), v)
for (i in 1:4) print(f2(i))
f3 <- function(n = stop("never")) 1
for (i in 1:4) print(f3())
f4 <- function(x = x) x
print(tryCatch(f4(), error = function(e) conditionMessage(e)))
f5 <- function(x = z) g5(x); g5 <- function(y) missing(y)
h5 <- function(z) f5x(z); f5x <- function(z, x = z) g5(x)
for (i in 1:3) print(h5())
for (i in 1:3) print(f5x(1))
f6 <- function(a, b = a + 1) { s <- substitute(b); e <- environment(); list(s, b, get("b", e), exists("b", e)) }
for (i in 1:3) str(f6(i))
f7 <- function(a, k = a * 10) function() k + a
for (i in 1:3) print(f7(i)())
f8 <- function(v, n = length(v)) sapply(seq_len(n), function(i) v[i] * n)
for (i in 1:3) print(f8(c(1, 2, 3)))
f9 <- function(x, default = NULL) { default <- if (is.null(default)) x else default; eval(quote(default + x)) }
for (i in 1:3) print(f9(i))
f10 <- function(a, b = { cat("force b\n"); a }) { a <- a + 100; b }
for (i in 1:3) print(f10(i))
f11 <- function(x, ..., sep = paste0("<", x, ">")) paste(x, ..., sep = sep)
for (i in 1:3) print(f11("a", "b", i))
f12 <- function(n, acc = 0) if (n == 0) acc else f12(n - 1, acc + n)
print(f12(200))
f13 <- function(a, b = a) { delayedAssign("q", b); a <- 5; q }
for (i in 1:3) print(f13(i))
f14 <- function(x, m = match.arg(x, c("aa", "bb"))) m
for (i in 1:3) print(f14("a"))
f15 <- function(type = c("one", "two")) { type <- match.arg(type); type }
for (i in 1:3) print(f15())
f16 <- function(a, b = a) { rm(a); tryCatch(b, error = function(e) "gone") }
for (i in 1:3) print(f16(1))
f17 <- function(a, b = parent.frame()) identical(b, globalenv())
for (i in 1:3) print(f17(1))
f18 <- function(a, b = sys.call()) b
for (i in 1:3) print(f18(1))
f19 <- function(x, b = x) { f <- function() b; x <- 99; f() }
for (i in 1:3) print(f19(i))
f20 <- function(x, w = stop("boom")) tryCatch(w, error = function(e) x)
for (i in 1:3) print(f20(i))
f21 <- function(x, y = x) { on.exit(cat("exit", y, "\n")); invisible(NULL) }
for (i in 1:3) f21(i)
f22 <- function(x, y = x + 1) UseMethod("f22"); f22.default <- function(x, y = x + 2) y
for (i in 1:3) print(f22(i))
f23 <- function(x, y = x) { y; vapply(1:2, function(k) k + y, 0) }
for (i in 1:3) print(f23(i))
f24 <- function(df, n = nrow(df)) lapply(seq_len(n), function(i) df[i, , drop = FALSE])
for (i in 1:3) print(length(f24(data.frame(a = 1:3))))
f25 <- function(a, b = a) { force(b); environment() }
for (i in 1:3) print(sort(ls(f25(1))))
f26 <- function(a, b = a) do.call("sum", list(a, b))
for (i in 1:3) print(f26(i))
f27 <- function(a, b = a) Recall2(a, b); Recall2 <- function(...) nargs()
for (i in 1:3) print(f27(i))
f28 <- function(a, b = a) { g <- function(b) missing(b); g(b) }
for (i in 1:3) print(f28(i))
f29 <- function(x = 1, y = x) { x <- 2; y }
for (i in 1:3) print(f29())
