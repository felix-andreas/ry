# A recursive `[[<-` copies every list it reaches through a shared one.
x <- list(a = list(b = list(c = 1))); y <- x
y[[c("a", "b", "c")]] <- 2
print(c(x$a$b$c, y$a$b$c))
x <- list(a = list(b = list(c = 1))); y <- x
y[[c("a", "b", "d")]] <- 2
str(x); str(y)
x <- list(a = list(b = c(1, 2))); y <- x
y[[c("a", "b", 2)]] <- 9
print(x$a$b); print(y$a$b)
x <- list(a = list(b = list(c = 1, d = 2))); y <- x
y[[c("a", "b", "c")]] <- NULL
str(x); str(y)
x <- list(list(list(list(1)))); y <- x
y[[c(1, 1, 1, 1)]] <- "deep"
print(x[[c(1, 1, 1, 1)]]); print(y[[c(1, 1, 1, 1)]])
f <- function(v) { v[[c("a", "b", "c")]] <- 0; v }
x <- list(a = list(b = list(c = 1)))
r <- f(x); r <- f(x)
print(c(x$a$b$c, r$a$b$c))
z <- list(a = list(b = list(c = 1)))
z[[c("a", "b", "c")]] <- 5
print(z$a$b$c)

# A value of several elements cannot skip an NA subscript.
message_of <- function(expr) tryCatch({expr; "no error"}, error = function(e) conditionMessage(e))
x <- 1:3
print(message_of(x[NA] <- 1:2))
print(message_of(x[c(NA, 1)] <- 4:5))
print(message_of(x[c(TRUE, NA, FALSE)] <- 4:5))
print(message_of(x[NA_integer_] <- 4:5))
l <- list(1, 2)
print(message_of(l[c(NA, 1)] <- list(4, 5)))
x[NA] <- 9L; print(x)
x[c(NA, 2)] <- 7L; print(x)
print(message_of(x[c(NA, 2)] <- integer(0)))
g <- function(v) { v[c(NA, 1)] <- 4:5; v }
print(tryCatch(g(1:3), error = function(e) conditionCall(e)))
n <- c(a = 1, b = 2)
print(message_of(n[c("a", NA)] <- 1:2))
