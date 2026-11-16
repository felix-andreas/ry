# Complex assignments whose replacement is a primitive: `[<-`, `[[<-`, `$<-` and the attribute
# setters on targets compiled code does not update in a slot (a classed list, a call, an
# environment, a variable of an enclosing function, a nested target). Each runs several times
# so compiled code runs it too. The results, the copies, the evaluation order of subscripts and
# the calls errors and warnings report must match GNU R's.
run <- function(f, ...) {
  for (k in 1:4) f(...)
  f(...)
}
report <- function(expr) {
  tryCatch(
    withCallingHandlers(expr, warning = function(w) {
      cat("warning:", conditionMessage(w), "\n")
      print(conditionCall(w))
      invokeRestart("muffleWarning")
    }),
    error = function(e) {
      cat("error:", conditionMessage(e), "\n")
      print(conditionCall(e))
    }
  )
}

# A classed list without a replacement method, by each element operator.
classed <- function() {
  x <- structure(list(a = 1, b = 2), class = "plain")
  y <- x
  x$a <- 10
  x[["b"]] <- 20
  x["c"] <- 30
  x$d <- NULL
  x$b <- NULL
  list(unclass(x), unclass(y), class(x))
}
print(run(classed))

# The same class once it has methods: each is called with the promises GNU R builds.
with_methods <- function() {
  x <- structure(list(a = 1, b = 2), class = "traced")
  x$a <- 10
  x[["b"]] <- 20
  x["c"] <- 30
  unclass(x)
}
print(run(with_methods))
`$<-.traced` <- function(x, name, value) {
  cat("$<-", name, deparse(substitute(value)), nargs(), "\n")
  y <- unclass(x)
  y[[name]] <- value
  structure(y, class = "traced")
}
`[[<-.traced` <- function(x, i, value) {
  cat("[[<-", i, deparse(substitute(i)), deparse(sys.call()), "\n")
  y <- unclass(x)
  y[[i]] <- value
  structure(y, class = "traced")
}
`[<-.traced` <- function(x, i, value) {
  cat("[<-", i, missing(i), nargs(), "\n")
  y <- unclass(x)
  y[i] <- value
  structure(y, class = "traced")
}
print(run(with_methods))
rm(`$<-.traced`, `[[<-.traced`, `[<-.traced`)
print(run(with_methods))

# A call and a pairlist as the target.
language <- function() {
  code <- quote(f(x, y = 2))
  kept <- code
  code[[1]] <- as.name("g")
  code$y <- 3
  code[["z"]] <- quote(h(1))
  code[2] <- list(quote(w))
  code$y <- NULL
  arguments <- pairlist(a = 1, b = 2)
  arguments$a <- 5
  arguments[["c"]] <- 6
  body_code <- quote({ a; b })
  body_code[[3]] <- quote(c)
  list(code, kept, arguments, body_code)
}
print(run(language))
function_parts <- function() {
  f <- function(a, b = 2) a + b
  formals(f)$b <- 5
  body(f)[[1]] <- as.name("-")
  list(formals(f), body(f), f(10))
}
print(run(function_parts))

# An environment is changed where it is.
environments <- function() {
  e <- new.env()
  alias <- e
  e$a <- 1
  e[["b"]] <- 2
  e$a <- NULL
  classed <- structure(new.env(), class = "holder")
  classed$field <- "value"
  classed[["other"]] <- 3
  list(mget(c("a", "b"), envir = alias), sort(ls(classed)), class(classed))
}
print(run(environments))

# `<<-` through each operator, on a variable of the enclosing function and of the global
# environment, which keep their other holders unchanged.
enclosing <- function() {
  values <- c(a = 1, b = 2, c = 3)
  items <- list(p = 1)
  copy <- values
  update <- function(i, v) {
    values[i] <<- v
    values[["c"]] <<- v + 1
    items$q <<- v
    items[["r"]] <<- list(v)
    names(values)[1] <<- "first"
    attr(items, "tag") <<- i
    invisible()
  }
  for (k in 1:5) update(2, k)
  list(values, items, copy)
}
print(enclosing())
counter <- integer(3)
snapshot <- counter
bump <- function(i) {
  counter[i] <<- counter[i] + 1L
  invisible()
}
for (k in 1:6) bump(1 + k %% 3)
print(counter)
print(snapshot)

# Nested targets: the setter levels run from the outside in.
nested <- function() {
  x <- list(a = c(p = 1, q = 2), b = list(c = 1:3))
  y <- x
  names(x$a) <- c("r", "s")
  names(x$a)[2] <- "t"
  x$b$c[2] <- 20L
  x[["b"]][["d"]] <- "new"
  attr(x$a, "unit") <- "m"
  attr(attr(x, "meta"), "inner") <- 1
  dim(x$b$c) <- c(1, 3)
  dimnames(x$b$c)[[2]] <- c("u", "v", "w")
  levels(x$f)[2] <- "z"
  list(x, y)
}
print(tryCatch(run(nested), error = function(e) conditionMessage(e)))
nested_matrix <- function() {
  m <- matrix(1:6, 2)
  l <- list(m = m)
  l$m[2, ] <- 0L
  l$m[, 3] <- c(8L, 9L)
  l$m[1, 1] <- 100L
  attr(l$m, "note")[2] <- "b"
  list(l, m)
}
print(run(nested_matrix))

# Subscripts evaluate once, in order, after the value.
ordered <- function() {
  x <- 1:5
  m <- matrix(0, 2, 2)
  l <- list(v = 1:3)
  x[{ cat("i\n"); 2 }] <- { cat("v\n"); 9L }
  m[{ cat("row\n"); 1 }, { cat("column\n"); 2 }] <- { cat("value\n"); 1 }
  l$v[{ cat("inner\n"); 3 }] <- { cat("right\n"); 0L }
  attr(x, { cat("which\n"); "a" }) <- { cat("attribute\n"); TRUE }
  list(x, m, l)
}
print(run(ordered))

# Empty and named subscripts, and a subscript that is a formal nobody passed.
shapes <- function(x, i, j) {
  x[i, ] <- 0
  x[, j] <- 1
  x[[i, j]] <- 5
  x[] <- x + 1
  x
}
print(run(shapes, matrix(1:6, 2), 1, 2))
missing_subscript <- function(x, i) {
  x[i] <- 0
  x
}
print(run(missing_subscript, 1:3))
print(run(missing_subscript, structure(1:3, class = "plain")))
missing_element <- function(x, i) {
  x[[i]] <- 0
  x
}
report(run(missing_element, list(1, 2)))
dots_subscripts <- function(x, ...) {
  x[...] <- 0
  x
}
print(run(dots_subscripts, matrix(1:4, 2), 1, 2))
print(run(dots_subscripts, 1:3, 2))

# A subscript that assigns to the variable works on a copy.
index_assigns <- function() {
  x <- c(1, 2, 3)
  x[{ x[2] <- 30; 1 }] <- 10
  x
}
print(run(index_assigns))

# The value is the target itself.
self <- function() {
  l <- list(1, 2)
  l[[2]] <- l
  l$again <- l
  e <- quote(f(a))
  e[[2]] <- e
  list(l, e)
}
print(run(self))

# A locked binding keeps its value.
locked <- function() {
  x <- c(1, 2)
  lockBinding("x", environment())
  result <- tryCatch({ x[1] <- 5; "assigned" }, error = function(e) conditionMessage(e))
  list(result, x)
}
print(run(locked))

# Errors and warnings name the assignment, and a deeper level its own target.
failures <- function() {
  x <- 1:3
  l <- list(a = 1:3)
  f <- function() NULL
  report(x[[5, 6]] <- 1)
  report(x[5:6] <- 1:3)
  report(x[[1:2]] <- 1)
  report(x$a <- 1)
  report(l$a[[7, 8]] <- 1)
  report(names(l$a)[[5, 6]] <- "n")
  report(names(x) <- 1:5)
  report(dim(x) <- c(2, 2))
  report(l$a$b$c <- 1)
  report(attr(x, c("a", "b")) <- 1)
  report(levels(l$a) <- NULL)
  report(f$a <- 1)
  report(f[[1]] <- 1)
  report(x[["a", exact = TRUE]] <- "s")
  report(x[2, drop = TRUE] <- 5L)
  report(l[["a", exact = TRUE]] <- 5L)
  report(x[list()] <- 1)
  report(missing_variable$a <- 1)
  report(missing_variable[1] <<- 1)
  invisible()
}
run(failures)

# A handler sees the call while the replacement runs.
handler_call <- function() {
  x <- 1:3
  withCallingHandlers(
    x[5:6] <- 1:3,
    warning = function(w) {
      print(sys.call(-1))
      print(conditionCall(w))
      invokeRestart("muffleWarning")
    }
  )
  x
}
print(run(handler_call))

# A replacement closure between primitive levels keeps its own call.
`second<-` <- function(x, value) {
  cat(deparse(sys.call()), "\n")
  x[2] <- value
  x
}
mixed <- function() {
  l <- list(a = c(1, 2, 3))
  second(l$a) <- 20
  names(second(l$a)) <- "n"
  second(l$a)[1] <- 5
  l
}
print(tryCatch(run(mixed), error = function(e) conditionMessage(e)))

# S4 objects: a slot, and a class with a `[<-` method.
setClass("Holder", representation(items = "list", tag = "character"))
holder <- function() {
  h <- new("Holder", items = list(1, 2), tag = "t")
  h@items[[2]] <- "two"
  h@items$extra <- 3
  names(h@items)[1] <- "one"
  h@tag[2] <- "u"
  h
}
print(run(holder))
setClass("Stack", representation(values = "numeric"))
setMethod("[<-", "Stack", function(x, i, ..., value) {
  cat("Stack [<-", i, "\n")
  x@values[i] <- value
  x
})
stack <- function() {
  s <- new("Stack", values = c(1, 2, 3))
  s[2] <- 20
  s@values
}
print(run(stack))

# A data frame and a factor dispatch as before.
frames <- function() {
  d <- data.frame(a = 1:3, b = c("x", "y", "z"))
  d$a[2] <- 20L
  d[["c"]] <- d$a * 2
  d[2, "b"] <- "w"
  names(d)[1] <- "first"
  f <- factor(c("u", "v", "u"))
  f[2] <- "u"
  levels(f)[1] <- "U"
  list(d, f)
}
print(run(frames))
