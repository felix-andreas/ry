# stopifnot passes on all-TRUE logicals and reports the first failing expression.
print(withVisible(stopifnot(TRUE, c(TRUE, TRUE), logical(0), 1 < 2)))
for (expr in list(quote(stopifnot(TRUE, FALSE)), quote(stopifnot(c(TRUE, NA))), quote(stopifnot(1)),
                  quote(stopifnot("x")), quote(stopifnot(all.equal(1, 2))), quote(stopifnot("must be positive" = -1 > 0)),
                  quote(stopifnot(exprs = { TRUE; 2 > 3 })), quote(stopifnot(NULL)), quote(stopifnot(structure(TRUE, class = "flag"))))) {
  print(tryCatch(eval(expr), error = function(e) c(conditionMessage(e), deparse(conditionCall(e)))))
}
f <- function(x) { stopifnot(is.numeric(x), length(x) == 1, x > 0); x * 2 }
print(f(2)); print(tryCatch(f(-1), error = function(e) c(conditionMessage(e), deparse(conditionCall(e)))))
print(tryCatch(f("a"), error = conditionMessage))
# Every expression runs in order up to the first failure, and none after it.
print(tryCatch(stopifnot({ cat("one\n"); TRUE }, { cat("two\n"); FALSE }, { cat("three\n"); TRUE }), error = conditionMessage))
print(tryCatch(stopifnot(TRUE, stop("boom")), error = conditionMessage))
g <- function(...) stopifnot(...); print(g(TRUE, TRUE)); print(tryCatch(g(TRUE, FALSE), error = conditionMessage))
h <- function() stopifnot(); print(h())
