# Base functions with a native fast path finish the call from their prologue. Compiled code
# returns the prologue's value directly; the result, visibility and errors match the R body.
f <- function(type = c("a", "bb", "ccc")) { type <- match.arg(type); nchar(type) }
g <- function(x) { stopifnot(is.numeric(x), x > 0); x * 2 }
s <- 0
for (i in 1:300) s <- s + f("bb") + g(i)
print(s)
print(f())
print(withVisible(stopifnot(TRUE)))
print(withVisible(structure(1:3, class = "foo")))
h <- function(x) tryCatch(stopifnot("x must be positive" = x > 0), error = conditionMessage)
for (i in 1:50) r <- h(-i)
print(r)
print(h(1))
# A prologue whose argument runs other prologues while it is forced.
nested <- function(v) stopifnot(identical(match.arg(v, c("p", "q")), "q"), length(intersect(1:3, 2:5)) == 2L)
for (i in 1:100) nested("q")
print(nested("q"))
print(tryCatch(nested("p"), error = conditionMessage))
k <- function(n) { Sys.setenv(RY_ORACLE_PROLOGUE = n); Sys.getenv("RY_ORACLE_PROLOGUE") }
for (i in 1:50) k(as.character(i))
print(k("7"))
print(setdiff(1:5, 2:3)); print(Reduce(`+`, 1:10, accumulate = TRUE))
