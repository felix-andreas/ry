# isTRUE(), isFALSE() and unname() answer natively both when their argument is a value and when
# it is a call that their frame forces. Results, attributes, visibility, errors and what the
# argument code sees of the stack match the R bodies.
show <- function(value) { print(value); invisible(value) }
attempt <- function(expr) tryCatch(show(expr), error = function(e) cat("error:", conditionMessage(e), "\n"))
x <- 3.5
for (i in 1:3) {
  show(isTRUE(!is.na(x)))
  show(isFALSE(!is.na(x)))
  show(isTRUE(is.na(x)))
  show(!isTRUE(!is.na(unname(x))))
}

# isTRUE() and isFALSE() of calls that give every kind of value.
show(isTRUE(c(TRUE, TRUE) & TRUE))
show(isTRUE(NA & TRUE))
show(isFALSE(NA & TRUE))
show(isTRUE(c(a = TRUE) & TRUE))
show(isFALSE(c(a = FALSE) | FALSE))
show(isTRUE(matrix(TRUE) & TRUE))
show(isTRUE(identity(1)))
show(isTRUE(identity("TRUE")))
show(isTRUE(identity(NULL)))
show(isFALSE(identity(0)))
show(isTRUE(logical(0) & TRUE))
show(isTRUE(identity(list(TRUE))))
flag <- function(value) structure(value, class = "flag")
show(isTRUE(flag(TRUE)))
show(isFALSE(flag(FALSE)))
length.flag <- function(x) 2L
show(isTRUE(flag(TRUE)))
show(isFALSE(flag(FALSE)))
rm(length.flag)
show(isTRUE(identity(factor("a"))))
setClass("Truth", representation("logical"))
show(isTRUE(identity(new("Truth", TRUE))))
setMethod("length", "Truth", function(x) 5L)
show(isTRUE(identity(new("Truth", TRUE))))
show(isTRUE(identity(structure(1, class = "flag"))))

# The argument is forced once, inside the closure's frame.
count <- 0
side <- function(value) { count <<- count + 1; value }
for (i in 1:5) isTRUE(side(TRUE))
show(count)
inside <- function() { print(vapply(sys.calls(), function(call) deparse(call)[[1]], "")); TRUE }
show(isTRUE(inside()))
show(isFALSE(!inside()))
first <- function() as.character(sys.call(sys.parent())[[1]])
show(isTRUE(first() == "isTRUE"))
g <- function(a) isTRUE(missing(a))
show(g())
show(g(1))
show(tryCatch(isTRUE(stop("in argument")), error = function(e) deparse(conditionCall(e))))
show(tryCatch(isTRUE(), error = conditionMessage))
show(withCallingHandlers(isTRUE({ warning("in argument"); TRUE }),
  warning = function(w) { print(conditionCall(w)); invokeRestart("muffleWarning") }))
show(withVisible(isTRUE(invisible(TRUE))))
show(isTRUE(x = !is.na(x)))

# unname() of vectors, lists, matrices, arrays, data frames and objects.
show(unname(c(a = 1, b = 2)))
show(unname(list(a = 1, b = "x")))
show(unname(1:3))
show(unname(NULL))
show(unname(structure(1:2, names = c("a", "b"), extra = "kept")))
m <- matrix(1:4, 2, dimnames = list(c("r1", "r2"), c("c1", "c2")))
show(unname(m))
show(unname(array(1:3, 3, dimnames = list(c("a", "b", "c")))))
show(unname(array(1:8, c(2, 2, 2), dimnames = list(NULL, c("p", "q"), NULL))))
show(unname(structure(1:4, dim = c(2L, 2L), names = letters[1:4])))
df <- data.frame(a = 1:2, b = 3:4)
attempt(unname(df))
attempt(unname(df, force = TRUE))
attempt(unname(m, TRUE))
attempt(unname(factor(c(a = "x"))))
show(unname(c(a = 1i)))
attempt(unname(expression(a = 1)))
attempt(unname(sum))
show(tryCatch(is.environment(unname(new.env())), error = conditionMessage))
attempt(unname(quote(f(a = 1))))
attempt(unname(pairlist(a = 1)))
tagged <- structure(list(a = 1), class = "tagged")
`names<-.tagged` <- function(x, value) { cat("names<- method\n"); x }
attempt(unname(tagged))
dimmed <- structure(list(1), class = "dimmed")
dimnames.dimmed <- function(x) list("only")
`dimnames<-.dimmed` <- function(x, value) { cat("dimnames<- method\n"); x }
attempt(unname(dimmed))

# The value unname() returns is a copy; the argument keeps its names.
named <- c(a = 1, b = 2)
plain <- unname(named)
show(named)
show(plain)
for (i in 1:5) plain <- unname(named)
show(names(named))
show(identical(unname(x), x))
show(unname(c(a = 1) + 1))
show(unname(paste(c(a = "p"))))
show(withVisible(unname(invisible(c(a = 1)))))
show(unname(obj = c(a = 1)))
show(tryCatch(unname(), error = conditionMessage))

# A user's isTRUE() and unname() mask base's.
local({
  isTRUE <- function(x) "mine"
  unname <- function(obj, force = FALSE) "mine too"
  show(isTRUE(!is.na(x)))
  show(unname(c(a = 1)))
})
