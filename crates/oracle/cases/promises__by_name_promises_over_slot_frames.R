# A promise whose code runs by name over a frame that keeps its variables in slots: the
# promise is made through `eval` in the frame, and the frame goes on assigning afterwards.
g <- function(v) v * 2
callee <- function() eval(quote(g(x + 1)), parent.frame())
outer1 <- function(x) { y <- callee(); y + x }
for (i in 1:5) print(outer1(i))
h <- function(p) function() p
outer2 <- function(x) { k <- eval(quote(h(x * 3))); x <- x + 100; k() + x }
for (i in 1:5) print(outer2(i))
lazy <- function(a, b) if (a > 2) b else 0
outer3 <- function(x) { r <- eval(quote(lazy(x, x <- x + 10))); c(r, x) }
for (i in 1:4) print(outer3(i))
outer4 <- function(x, n) { total <- 0; for (i in seq_len(n)) total <- total + callee(); total }
print(outer4(2, 50))
delayed <- function(x) { delayedAssign("later", x * 2); x <- 10; later }
for (i in 1:3) print(delayed(i))
forced_late <- function(x) { f <- function(p) { x <<- 50; p }; f(x + 1) }
for (i in 1:3) print(forced_late(i))
