# GNU R checks an assignment with two or more subscripts in a fixed order: the number of
# subscripts, each dimension's subscript in turn, the value's length, and the value's type last.
# So a call that is wrong in two ways reports the earlier check.
report <- function(expression) {
  tryCatch({
    expression
    "ok"
  }, error = function(condition) conditionMessage(condition))
}
raw_value <- as.raw(1)
m <- matrix(1:6, 2)
a <- array(1:24, 2:4)
x <- c(1i, 2i)
empty <- matrix(0, 0, 8)

# `[<-`: the subscript count and each subscript come before the type mismatch.
report(x[1, 2] <- raw_value)
report(x[1, 2, 3] <- raw_value)
report(m[1, 2, 1] <- raw_value)
report(m[5, 2] <- raw_value)
report(m[1, 9] <- raw_value)
report(m[c(TRUE, FALSE, TRUE), 1] <- raw_value)
report(m[1, "a"] <- raw_value)
report(m[1, "a"] <- 1L)
report(m[1, "a"])
report(a[1, 1, 9] <- raw_value)
report(empty[c(TRUE, FALSE), 9] <- 1)

# The value's length comes before its type, and after the subscripts.
report(m[1, 1:2] <- as.raw(1:3))
report(m[1, 1:2] <- raw(0))
report(a[1, 1, 1:2] <- as.raw(1:3))
report(m[9, 1:2] <- as.raw(1:3))
report(m[1, 1] <- raw_value)

# `[[<-` reads the value's length first for an atomic matrix, then each subscript as `[[` does,
# so a subscript must name exactly one element.
report(m[[9, 1]] <- 1:2)
report(m[[1, 1]] <- integer(0))
report(m[[1:2, 1]] <- 1L)
report(m[[1, 1:2]] <- 1L)
report(a[[1, 1, 1:2]] <- 1L)
report(m[[0, 1]] <- 1L)
report(m[[0L, 1]] <- 1L)
report(m[[NA, 1]] <- 1L)
report(m[[9, 1]] <- 1L)
report(m[["a", 1]] <- 1L)
report(m[[1, 1, 1]] <- 1L)
report(m[[9, 1]] <- raw_value)
report(m[[1, 1]] <- raw_value)
failure <- tryCatch(m[[1:2, 1]] <- 1L, error = function(condition) condition)
conditionCall(failure)
failure <- tryCatch(m[[9, 1]] <- 1L, error = function(condition) condition)
class(failure)
conditionCall(failure)

# A logical subscript names the first element instead of recycling, and a negative one names
# the other of two.
m[[TRUE, 2]] <- 70L
m[[-1, 3]] <- 80L
m

# A list matrix takes any value in a cell except `NULL`, and a list or expression value turns
# an atomic matrix into a vector of that kind without dimensions.
l <- matrix(list(1, 2, 3, 4), 2)
report(l[[9, 1]] <- 1:2)
report(l[[1, 2]] <- NULL)
l[[2, 2]] <- 1:3
l[[1, 1]] <- sum
str(l)
m2 <- matrix(1:4, 2)
m2[[2, 2]] <- list("a")
str(m2)
m3 <- matrix(1:4, 2)
m3[[2, 2]] <- expression(b)
m3
e <- expression(a, b, c, d)
dim(e) <- c(2, 2)
report(e[[1, 1]] <- NULL)
report(e[[1, 1]] <- list(1))
report(e[[1, 1]] <- raw_value)
e[[1, 2]] <- quote(z)
e

# A function, an environment and a symbol have no length, and GNU R counts one that no
# selection can hold, so only an empty selection reaches the check of the type. A call counts
# its elements.
holder <- new.env()
l <- matrix(list(1, 2, 3, 4), 2)
plain <- list(1, 2, 3)
for (value in list(sum, function(a) a, quote, holder, quote(s), quote(f(1, 2)))) {
  cat(typeof(value), "\n")
  cat(" list matrix:", report(l[1, 2] <- value), "\n")
  cat(" list column:", report(l[, 2] <- value), "\n")
  cat(" list matrix, nothing selected:", report(l[0, 2] <- value), "\n")
  cat(" integer matrix:", report(m[1, 2] <- value), "\n")
  cat(" integer matrix, nothing selected:", report(m[0, 2] <- value), "\n")
  cat(" integer array:", report(a[, 1, 1] <- value), "\n")
  cat(" list:", report(plain[2] <- value), "\n")
}

# `NULL` becomes a vector of the value's type before any subscript is read, which no value
# without a length allows, except an environment that binds nothing.
for (value in list(sum, function(a) a, holder, quote(s), quote(f(1, 2)), pairlist(1))) {
  cat(typeof(value), "\n")
  failure <- tryCatch({ x <- NULL; x[3] <- value; x }, error = function(condition) condition)
  if (is.null(failure)) {
    cat(" one subscript: NULL\n")
  } else {
    cat(" one subscript:", conditionMessage(failure), "\n in", deparse(conditionCall(failure)), "\n")
  }
  cat(" nothing selected:", report({ x <- NULL; x[0] <- value }), "\n")
  cat(" two subscripts:", report({ x <- NULL; x[1, 1] <- value }), "\n")
  cat(" nested:", report({ x <- list(); x[["new"]]$part[3] <- value }), "\n")
}
assign("bound", 1, holder)
report({ x <- NULL; x[3] <- holder })

# `NULL` through two or more subscripts counts as a value without a length for every type of
# matrix, and through one subscript it is a value of length zero, for a raw vector too.
for (values in list(as.raw(1:8), 1:8, letters[1:8], as.list(1:8))) {
  cat(typeof(values), "\n")
  x <- values
  dim(x) <- c(2, 4)
  cat(" cell:", report(x[1, 2] <- NULL), "\n")
  cat(" column:", report(x[, 2] <- NULL), "\n")
  cat(" nothing selected:", report(x[0, 2] <- NULL), "\n")
  cat(" by position:", report(x[3] <- NULL), dim(x), "\n")
  cat(" by matrix:", report(x[cbind(1, 2)] <- NULL), "\n")
  dim(x) <- c(2, 2, 2)
  cat(" array cell:", report(x[1, 2, 1] <- NULL), "\n")
}
bytes <- as.raw(1:6)
report(bytes[4L] <- NULL)
report(bytes[c(2, 2)] <- NULL)
bytes[0] <- NULL
bytes
