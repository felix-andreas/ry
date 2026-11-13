# A closure's formals belong to the closure, not its body, so one compiled body can meet
# different formals. A call site plans for one pair of formals and body, and binding must
# follow the formals the closure has at each call. Each function runs many times so its body
# compiles and its call sites plan before the formals change.

# formals<- after the closure was called and compiled: adding a default, adding `...`, and
# turning a literal default into code.
target <- function(a, b) if (missing(b)) c(a, -1) else c(a, b)
show <- function(a, b) c(a, b, missing(b))
call_target <- function(v) target(v)
for (i in 1:20) r1 <- call_target(i)
formals(target) <- alist(a = , b = 7)
for (i in 1:20) r2 <- call_target(i)
formals(target) <- alist(a = , b = 7, ... = )
for (i in 1:20) r3 <- list(call_target(i), target(1, 2, 3))
formals(target) <- alist(a = , b = a * 10, ... = )
for (i in 1:20) r4 <- call_target(i)
str(list(r1, r2, r3, r4))
body(target) <- body(show)
for (i in 1:20) r5 <- list(call_target(i), target(i, 3))
formals(target) <- alist(a = , b = 7)
for (i in 1:20) r6 <- call_target(i)
str(list(r5, r6))

# Two closures over one body with different formals, called from the same site in turn.
body_shared <- quote(if (missing(y)) x else x + y)
first <- as.function(c(alist(x = , y = ), body_shared))
second <- as.function(c(alist(x = , y = 100), body_shared))
third <- as.function(c(alist(... = , x = 5, y = ), body_shared))
alternate <- function(f, v) f(v)
for (i in 1:30) r <- list(alternate(first, i), alternate(second, i), alternate(third, i))
str(r)

# The same through a site that names its argument, so matching depends on the formals' order.
named <- function(f) f(x = 2)
for (i in 1:30) r <- list(named(first), named(second), named(third), third(1, 2, y = 3))
str(r)
