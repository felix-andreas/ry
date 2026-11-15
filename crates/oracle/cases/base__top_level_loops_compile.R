# A loop the tree walker evaluates in the global environment compiles when it starts, as GNU R
# compiles every loop there. The compiled loop must behave as the tree walker does.
for (i in 1:100) last <- i
print(c(i, last))
for (j in integer(0)) 1
print(j)
for (x in factor(rep(c("a", "b"), 50))) last <- x
print(last)
for (x in as.list(1:100)) last <- x
print(last)
total <- 0
for (i in seq(0.5, 100, by = 0.5)) total <- total + i
print(total)
n <- 0
while (n < 200) n <- n + 1
print(n)
n <- 0
repeat { n <- n + 1; if (n == 150) break }
print(n)
s <- 0
for (i in 1:100) { if (i %% 2 == 0) next; s <- s + i }
print(s)
n <- 0
while (TRUE) { n <- n + 1; if (n < 100) next; if (n == 300) break }
print(n)
for (i in 1:100) { tryCatch(if (i == 70) break, error = function(e) NULL) }
print(i)
for (i in 1:100) { eval(quote(if (i == 80) break)) }
print(i)
f <- function() break
r <- tryCatch(for (i in 1:100) if (i == 90) f(), error = function(e) conditionMessage(e))
print(r)
r <- tryCatch(for (i in 1:100) if (i == 70) undefined_variable, error = function(e) e)
print(conditionCall(r))
print(conditionMessage(r))
withCallingHandlers(
  for (i in 1:100) if (i == 70) log(-1),
  warning = function(w) { print(conditionCall(w)); invokeRestart("muffleWarning") }
)
g <- function() print(identical(parent.frame(), globalenv()))
for (i in 1:100) if (i == 50) g()
h <- function() sys.call(-1)
k <- function() h()
for (i in 1:100) if (i == 50) print(k())
s <- 0
for (i in 1:100) { if (i == 50) `+` <- function(a, b) base::`-`(a, b); s <- s + 1 }
rm(`+`)
print(s)
v <- 1:100
for (i in v) v[i] <- 0L
print(sum(v))
u <- c(1, 2, 3)
for (i in 1:100) u[2] <- i
print(u)
counter <- 0
bump <- function() counter <<- counter + 1
for (i in 1:100) bump()
print(counter)
for (i in 1:100) if (i == 100) on.exit(cat("exit\n"))
cat("after on.exit\n")
rm(x)
r <- tryCatch(for (i in 1:100) if (i == 5) missing(x), error = function(e) conditionMessage(e))
print(r)
r <- withVisible(for (i in 1:100) i)
print(r)
