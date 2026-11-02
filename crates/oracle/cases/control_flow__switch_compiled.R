# switch inside closures, which ry compiles to bytecode; each shape runs repeatedly so GNU R's
# byte compiler sees it too.
f <- function(x) switch(x, a = "A", b = , c = "BC", d = , "default")
for (x in list("a", "b", "c", "d", "e", NA_character_, "", 1, 2, 3, 4, 5, 6, 0, -1, 2.7, TRUE, NA)) {
  r <- withVisible(tryCatch(f(x), error = function(e) conditionMessage(e)))
  cat(deparse(x), "->", deparse(r$value), r$visible, "\n")
}
g <- function(x) switch(x, a = 1, b = 2)
print(withVisible(g("z")))
print(withVisible(g(3)))
h <- function(x) switch(x, "one")
print(h(2)); print(h("one")); print(withVisible(h("x")))
e <- function(x) tryCatch(switch(x, a = 1), error = function(e) conditionCall(e))
print(e(c("a", "b"))); print(e(character()))
k <- function(x) { n <- 0; switch(x, a = { n <- n + 1 }, b = { n <- n + 10 }); n }
print(k("a")); print(k("b")); print(k("c"))
dd <- function(x) switch(x, a = 1, 2, 3)
print(tryCatch(dd("z"), error = function(e) conditionMessage(e)))
print(dd("a"))
m <- function(x) switch(x, a = invisible(5), b = 6)
print(withVisible(m("a"))); print(withVisible(m("b")))
emp <- function(x) switch(x, a = , b = )
print(withVisible(emp("a")))
num <- function(i) tryCatch(switch(i, "x", , "z"), error = function(e) conditionMessage(e))
print(num(1)); print(num(2)); print(num(3))
fac <- function(x) switch(x, "p", "q")
print(fac(factor("b", levels = c("a", "b"))))
sel <- function(x) switch(x, `NA` = "na-arm", "dflt")
print(sel(NA_character_)); print(sel("NA"))
nested <- function(x, y) switch(x, a = switch(y, p = "ap", "a?"), b = "b")
print(nested("a", "p")); print(nested("a", "q")); print(nested("b", "p"))
loop <- function() { out <- character(); for (s in c("a","b","x")) out <- c(out, switch(s, a = "A", b = "B", "?")); out }
print(loop())
side <- function() { i <- 0; s <- switch({ i <- i + 1; "a" }, a = i); s }
print(side())
nx <- function() { x <- 0; for (i in 1:3) switch("a", a = { if (i == 2) next; x <- x + i }); x }
print(nx())
t <- function(e) tryCatch(e, error = function(e) conditionMessage(e), warning = function(w) paste("W:", conditionMessage(w)))
f <- function(x) switch(x, "one")
for (i in 1:3) print(t(f("a")))
g <- function(x) switch(x, b = 1, "one", "two")
for (i in 1:3) print(t(g("b")))
h <- function(x) switch(x)
for (i in 1:3) print(t(h("b")))
