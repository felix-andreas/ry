# A missing argument's error is a `missingArgError`, whose first class names what read the
# argument and whose call is the function the argument belongs to. GNU R's first class is
# `evalError` in its AST evaluator and `getvarError` in compiled code, so `show` prints one
# name for both.
show <- function(expr) {
  e <- tryCatch(expr, error = function(e) e)
  first <- class(e)[1]
  if (first %in% c("evalError", "getvarError")) first <- "evalError or getvarError"
  cat(first, "|", paste(class(e)[-1], collapse = " "), "|", conditionMessage(e), "|",
      paste(deparse(conditionCall(e)), collapse = " "), "\n")
}

f <- function(a) g(a)
g <- function(b) b
show(f())
show((function(x) x + 1)())
show((function(x) identity(x))())
show((function(x) cat(x))())
show((function(x) (function(y) y)(x))())
show((function(x) tryCatch(x, finally = 1))())
show((function(x) vapply(1, function(i) x, 1))())

show((function(x) get("x"))())
show((function(x) get0("x"))())
show((function(f) f(1))())
show((function(x) log(x))())
show((function(x) log(x, 2))())
show(log(, 2))
show((function(x) round(x))())
show((function(x) signif(x, 2))())

show((function(...) ..2)(1, ))
show((function(...) ...elt(2))(1, ))
show((function(...) list(...))(1, ))
show((function(...) c(...))(, 1))

show((function(x) rep(x, 2))())
show((function(x) rep(x))())
show((function(...) rep(...))(, 2))
show((function(x) rep(times = 2, x))())
show((function(x) .subset(x, 1))())
show((function(x) .subset2(x, 1))())
show((function(x) .subset(1:3, x))())
show(.subset(1:3, ))

show((function(x) { x[1] <- 2; x })())
show((function(x) { x[[1]] <- 2; x })())
show((function(x) { x$a <- 2; x })())
show((function(x) { names(x) <- "a"; x })())
show((function(x) { attr(x, "a") <- 1; x })())
show((function(x) { length(x) <- 2; x })())
show((function(x) (function() x[1] <<- 2)())())

show((function(x) Exec(x))())
show((function() Tailcall())())

# A missing optional argument is not an error where the function keeps it.
print((function(x) log(2, x))())
print(log(2, ))
print((function(x, y) log(x, y))(8))
print((function(x) rep(1:2, x))())
print(rep())

# Handlers select by the class, and the condition carries a message and a call only.
print(tryCatch(f(), missingArgError = function(e) "missingArgError handler"))
print(names(tryCatch(f(), error = function(e) e)))
withCallingHandlers(
  tryCatch(f(), error = function(e) cat("exiting handler\n")),
  missingArgError = function(e) cat("calling handler:", inherits(e, "error"), "\n")
)
print(inherits(tryCatch(stop("x"), error = function(e) e), "missingArgError"))
print(geterrmessage() == geterrmessage())
r <- try(f(), silent = TRUE)
cat(r)
print(class(attr(r, "condition"))[-1])

# `...elt` reports its other errors as `..N` does.
show((function() ...elt(1))())
show((function(...) ...elt(3))(1, 2))
show((function(...) ...elt(0))(1, 2))
show((function(...) ...elt(1))())
