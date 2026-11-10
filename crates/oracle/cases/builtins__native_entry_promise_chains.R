# A promise passed on through wrappers into a base closure with a native entry: the chain is
# read without running code, and a chain that ends in code is forced inside the closure's frame.
g <- function(x) typeof(x)
h <- function(y) g(y)
v <- 1L
print(h(v)); print(g(v)); print(h(2.5)); print(h("a"))
loud <- function(value) { cat("forced\n"); value }
print(h(loud(1)))
same <- function(a, b) identical(a, b)
both <- function(p, q) same(p, q)
print(both(loud(1), { cat("second\n"); 1 }))
w <- c(1, 2)
print(both(w, c(1, 2))); print(both(w, list(1, 2)))
# Forcing runs inside the closure's frame, so the forced code sees the closure as its caller.
who <- function() { print(sys.call(-1)); 1L }
print(h(who()))
print(both(who(), 1L))
calls <- function() { stack <- sys.calls(); for (call in tail(stack, 3L)) print(call); 1L }
print(h(calls()))
# Each promise on a chain is settled: a later change to the variable is not seen.
z <- 1
k <- function(u) { first <- typeof(u); z <<- "changed"; c(first, typeof(u), u) }
print(k(z))
z <- 1
k2 <- function(u) { j <- function(t) typeof(t); first <- j(u); z <<- "changed"; c(first, u) }
print(k2(z))
# A promise the closure never forces stays lazy.
z <- 1
lazy <- function(u) { r <- getOption("digits", u); z <<- 2; c(r, u) }
print(lazy(z))
z <- 1
print(lazy(z))
unset <- function(u) { r <- getOption("ry.oracle.unset", u); z <<- 3; c(r, u) }
z <- 1
print(unset(z))
z <- 1
print(unset(z))
either <- function(a, u) { r <- a %||% u; z <<- 2; c(r, u) }
z <- 1
print(either(0, z)); z <- 1; print(either(0, z))
z <- 1
print(either(NULL, z)); z <- 1; print(either(NULL, z))
# inherits and is.vector through two layers.
inh <- function(x) inherits(x, "data.frame")
inh2 <- function(x) inh(x)
df <- data.frame(a = 1)
print(inh2(df)); print(inh2(1)); print(inh2(loud(df)))
vec <- function(x) is.vector(x)
vec2 <- function(x) vec(x)
print(vec2(1:3)); print(vec2(df)); print(vec2(loud(list(1))))
# A chain longer than a few links, and a chain that loops back on itself.
deep <- function(x, n) if (n == 0) typeof(x) else deep(x, n - 1)
print(deep(v, 25)); print(deep(loud(1i), 25))
loop <- function(x = y, y = x) typeof(x)
print(tryCatch(loop(), error = conditionMessage))
# A missing argument passed on fails as forcing it does.
miss <- function(m) g(m)
print(tryCatch(miss(), error = conditionMessage))
