# An error GNU R raises while it reads one dimension's subscript of `m[i, j] <- value` names the
# replacement call the assignment generated, where the same error in `m[i, j]` names that call.
# A value that is not a vector (a call, a pairlist, a function) fits no selection but an empty one,
# and a symbol subscript of `[[` names an element as its string does.
report <- function(expression) {
  tryCatch({
    expression
    "ok"
  }, error = function(condition) {
    paste(conditionMessage(condition), "|", paste(deparse(conditionCall(condition)), collapse = " "))
  })
}
m <- matrix(1:6, 2, dimnames = list(c("a", "b"), NULL))
n <- matrix(1:6, 2)
report(m[c(TRUE, FALSE, TRUE), 1] <- 1L)
report(m[c(TRUE, FALSE, TRUE), 1])
report(n["a", 1] <- 1L)
report(n["a", 1])
report(m[list(1), 1] <- 1L)
report(m[list(1), 1])
report(m[c(-1, 1), 1] <- 1L)
report(m[1, "z"] <- 1L)
report(m[9, 1] <- 1L)
report(m[[1:2, 1]] <- 1L)
report(m[[list(1), 1]] <- 1L)
report(m[["z", 1]] <- 1L)

report(m[1, 1:2] <- quote(f(x)))
report(m[1, 1:2] <- pairlist(1, 2))
report(m[1, 1] <- pairlist(1))
report(m[1, 1] <- quote(x))
report(m[1, 1] <- sum)
report(m[0, 1] <- quote(f(x)))
l <- matrix(list(1, 2, 3, 4), 2)
report(l[1, 1:2] <- pairlist(1, 2))
report(l[1, 1:2] <- quote(f(x)))
report(l[1, 1] <- sum)
e <- expression(a, b, c, d)
dim(e) <- c(2, 2)
report(e[1, 1:2] <- quote(f(x)))
str(l)

v <- list(a = 1, 2)
v[[quote(a)]]
v[[quote(b)]]
w <- c(a = 1)
report(w[[quote(a)]])
report(w[[quote(b)]])
m[[quote(a), 1]]
m[[quote(b), 2]] <- 40L
m
v[[quote(b)]] <- 3
str(v)
