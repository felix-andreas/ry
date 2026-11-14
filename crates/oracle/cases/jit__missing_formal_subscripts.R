# A formal passed on as a subscript keeps its missingness: a missing one is an empty subscript,
# a defaulted one is its default. The loops run long enough for compiled code to take over.
f <- function(x, i) x[i]
g <- function(i = 2) f(c(10, 20, 30), i)
g2 <- function(i) f(c(10, 20, 30), i)
h <- function(x, i, j) x[[i, j]]
m <- matrix(1:4, 2)
k <- function(i = 2) { force(i); f(c(10, 20, 30), i) }
k2 <- function(i = 2) { i <- 3; f(c(10, 20, 30), i) }
print(g())
print(g2())
print(f(1:3))
print(tryCatch(h(m, 1), error = function(e) conditionMessage(e)))
print(k())
print(k2())
for (n in 1:400) { g(); g2(); k(); k2(); h(m, 1, 2) }
print(g()); print(g2()); print(k()); print(k2()); print(h(m, 2, 2))
print(tryCatch(h(m, 1), error = function(e) conditionMessage(e)))

# The same closure sees a present subscript for hundreds of calls and then a missing one.
element <- function(x, i) x[i]
total <- 0
for (n in 1:400) total <- total + element(c(1, 2, 3), 2)
print(total)
print(element(c(1, 2, 3)))
through <- function(x, i) element(x, i)
for (n in 1:400) total <- total + through(c(1, 2, 3), 3)
print(total)
print(through(c(1, 2, 3)))

# A one-line matrix accessor called from a hot loop, with closures made afresh on each call.
run <- function(n) {
    cache <- matrix(as.double(seq_len(n * n)), n)
    at <- function(i, j) cache[[i, j]]
    sum_all <- function() {
        s <- 0
        for (i in 1:n) for (j in 1:n) s <- s + at(i, j)
        s
    }
    sum_all()
}
for (round in 1:4) print(run(30))
