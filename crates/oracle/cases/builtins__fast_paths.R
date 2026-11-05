# Edge cases of the builtin fast paths: each result must match GNU R's general path exactly.

# attributes(): names first, pairlist names, row.names expanded, environments without names.
x <- structure(1:3, names = c("a", "b", NA), foo = list(1), class = "bar")
str(attributes(x))
attributes(structure(1:2, dim = 2L, names = c("p", "q")))
attributes(pairlist(a = 1, 2))
attributes(data.frame(u = 1:3, v = c("a", "b", "c")))
attributes(data.frame(u = 1:2, row.names = c("r1", "r2")))
attributes(1:3)
e <- new.env()
attr(e, "tag") <- "t"
attributes(e)
attributes(structure(list(), names = character()))
attributes(quote(f(a = 1)))

# c(): widening integer and logical into double and integer, NA kept, names joined.
c(1:3, 4.5)
c(TRUE, NA, 2L)
c(NA, 1.5)
c(NA_integer_, 2)
c(a = 1L, b = NA, 3.5)
c(TRUE, FALSE, NA, 1i)
c(c(x = 1, y = 2), z = 3)
c(list(a = 1), b = 2, list(3, c = 4))
c(a = c(b = 1, 2), c = list(d = 3))
c(c(NA_character_), x = "y")
c(list(), NULL, character())
c(1, list(2), recursive = TRUE)
c(list(a = list(b = 1, c = list(d = 2))), recursive = TRUE)
c(list(1, list(2, "x")), recursive = TRUE, use.names = FALSE)
c(x = list(1, 2), recursive = TRUE)
c(x = list(a = 1, 2), recursive = TRUE)
c(x = c(1, 2), y = 3)
names(c(x = c(a = 1, 2)))
nm <- c(1, 2)
names(nm) <- c("a", NA)
c(k = nm)
names(unlist(list(a = 1:2, b = list(c = 3, 4), NA)))
unlist(list(list(a = 1), list(a = 2)), use.names = FALSE)
unlist(list(a = list(1), b = list(x = 2)))

# forceAndCall() and Reduce(): promises, substitute, missing arguments, dots.
f <- function(a, b) a + b
forceAndCall(2, f, 1, 2)
xs <- 3
ys <- 4
forceAndCall(2, f, xs, ys)
forceAndCall(1, function(a, b) substitute(b), xs, ys + 1)
forceAndCall(2, function(a, b) substitute(a), xs, ys)
forceAndCall(1, function(a, b) missing(b), xs)
forceAndCall(2, function(...) sum(...), 1, 2, 3)
g <- function(...) forceAndCall(1, function(a, ...) list(a, ...), ...)
g(1, b = 2)
forceAndCall(2, `+`, 1, 2)
forceAndCall(1, sum, 1, 2, na.rm = TRUE)
Reduce(function(a, b) paste0(a, b), letters[1:5])
Reduce(function(a, b) paste0(a, b), letters[1:5], accumulate = TRUE, right = TRUE)
Reduce(f, list(), 0)
Reduce(function(a, b) substitute(b), 1:3)
counter <- 0
Reduce(function(a, b) { counter <<- counter + 1; a * b }, 1:6)
counter
tryCatch(forceAndCall(1, function(a) a, stop("boom")), error = conditionMessage)

# vapply() and lapply() with builtin and special functions.
rows <- list(list(a = 1, fee = 2), list(a = 3, fee = 4), list(a = 5))
vapply(rows[1:2], `[[`, numeric(1), "fee")
lapply(rows, `[[`, "fee")
vapply(rows, `[[`, numeric(1), 1)
lapply(list(1:3, 4:6), `[`, 2)
vapply(list(a = 1:3, b = 4:6), sum, numeric(1))
vapply(list(1:3, c(NA, 1L)), sum, integer(1), na.rm = TRUE)
lapply(list(a = 1, b = "x"), is.numeric)
lapply(list(structure(list(v = 1), class = "rec")), `[[`, "v")
`[[.rec` <- function(x, i) paste("method", i)
lapply(list(structure(list(v = 1), class = "rec")), `[[`, "v")
rm(`[[.rec`)
tryCatch(vapply(rows, `[[`, numeric(1), "fee"), error = conditionMessage)
lapply(c(a = 1, b = 2), `-`)
sapply(list(1:2, 3:4), `[[`, 2)

# [[ with constant subscripts on plain lists, and [[<- by name.
l <- list(a = 1, b = NULL, fee = 3, fee = 4)
l[["fee"]]
l[[3]]
l[["missing"]]
l[["fe"]]
l[[2]]
tryCatch(l[[9]], error = conditionMessage)
nl <- list(1, 2)
nl[["a"]]
names(l)[1] <- NA
l[["NA"]]
l[[1]]
l2 <- list(a = 1, fee = 2)
l2[["net"]] <- 3
l2[["fee"]] <- 5
l2[["fee"]] <- NULL
str(l2)
l3 <- list(1, 2)
l3[[""]] <- 9
str(l3)
v <- c(a = 1, b = 2)
v["b"] <- 7
v[["c"]] <- 8
v
lp <- list(p = 1)
lp[["p"]] <- list(2)
str(lp)

# Reduce() without accumulate: the calls f sees, its frame variables, init, right and dispatch.
Reduce(function(a, b) sys.call(), 1:3)
Reduce(function(a, b) c(a, parent.frame()$i), 1:4)
Reduce(function(a, b) paste(a, b), c(p = "x", q = "y", r = "z"), right = TRUE)
Reduce(function(a, b) paste(a, b), letters[1:3], "init", right = TRUE)
Reduce(function(a, b) paste(a, b), letters[1:3], "init")
Reduce(`+`, 1:4, 100)
Reduce(`-`, list(10, 3, 2), right = TRUE)
Reduce("+", 1:4)
Reduce(function(a, b) list(a, b), list(NULL, 1, NULL))
Reduce(`+`, factor(1:3))
Reduce(function(a, b) a + b, 1:3, init = { cat("init forced\n"); 0 })
Reduce(function(a, b) substitute(a), 1:3, 0)
Reduce(function(a, b) substitute(b), 1:3, 0, right = TRUE)
Ops.money <- function(e1, e2) structure(unclass(e1) + unclass(e2), class = "money")
Reduce(`+`, list(structure(1, class = "money"), structure(2, class = "money"), 3))
rm(Ops.money)
Reduce(function(a, b) a * b, 1:5, accumulate = TRUE)
Reduce(function(a, b) a * b, 1:5, accumulate = NA)
tryCatch(Reduce(function(a, b) stop("at ", b), 1:3), error = conditionMessage)
Reduce(function(a, b) a + b, numeric(), 7)
Reduce(function(a, b) a + b, 2.5)
Reduce(max, c(3, NA, 1))
Reduce(function(a, b) a + b, structure(1:3, dim = 3L))
Reduce(function(a, b) { a[[length(a) + 1L]] <- b; a }, list("x", "y"), list())
