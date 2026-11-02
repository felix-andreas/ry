# Closure arguments that are calls compile to promise code the first time a call site reaches a
# closure. Each function runs several times so its body is compiled.
id <- function(v) v
deep <- function(x) id(id(id(id(id(id(id(id(id(id(id(id(id(id(id(id(id(id(id(id(x + 1))))))))))))))))))))
for (i in 1:3) print(deep(i))
nested <- function(a) ifelse(a > 5, 5, ifelse(a > 4, 4, ifelse(a > 3, 3, ifelse(a > 2, 2, 0))))
for (i in 1:3) print(nested(1:7))
# The same call site reaches a builtin, then closures.
g <- function(f, x) f(x * 2 + 1)
for (i in 1:3) print(g(sqrt, 4))
for (i in 1:3) print(g(function(z) z + 1, 4))
for (i in 1:3) print(g(function(z) substitute(z), 4))
for (i in 1:3) print(g(function(z) "never forced", stop("no")))
# Forcing order, side effects and assignment inside an argument.
trace <- character()
note <- function(label, value) { trace <<- c(trace, label); value }
pair <- function(a, b) b - a
run_pair <- function(i) pair(note("a", i * 10), note("b", i + 100))
for (i in 1:3) print(run_pair(i))
print(trace)
assign_in <- function() { y <- 1; id(y <- y + 41); y }
for (i in 1:3) print(assign_in())
# sys.call, match.call and missing see the expressions.
shown <- function(p, q) list(sys.call(), match.call(), missing(q))
run_shown <- function(i) shown(i + 1)
for (i in 1:3) print(run_shown(i))
# Errors raised while forcing.
fails <- function(n) tryCatch(id(stop("boom ", n * 2)), error = conditionMessage)
for (i in 1:3) print(fails(i))
# Recursion through promise arguments.
count <- function(n) if (n == 0) 0 else 1 + count(n - 1)
for (i in 1:3) print(count(i * 10))
