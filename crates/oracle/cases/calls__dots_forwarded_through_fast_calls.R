g <- function(...) { xs <- list(...); n <- 0; for (x in xs) n <- n + length(x); n }
f <- function(...) g(...)
h <- function(a, ...) f(a, paste0("k", a), ..., c(a, a + 1))
total <- 0
for (i in 1:300) total <- total + h(i, as.character(i), list(i, "x"), i * 2)
print(total)
k <- function(...) ..2
m <- function(...) k(...)
print(m(1, paste("a", "b"), 3))
nm <- function(...) names(list(...))
pass <- function(...) nm(...)
print(pass(a = 1, b = paste0("x", 2), 3))
