# eval() of an expression vector, the way testthat runs a test file, lets an element's
# dropped object be finalized by a gc() in a later element.
finalized <- FALSE
exprs <- parse(text = c(
  "x <- new.env()",
  "reg.finalizer(x, function(e) finalized <<- TRUE)",
  "rm(x)",
  "invisible(gc())"
), keep.source = TRUE)
eval(exprs, new.env())
print(finalized)

finalized <- FALSE
eval(expression(y <- new.env(), reg.finalizer(y, function(e) finalized <<- TRUE), rm(y), invisible(gc())))
print(finalized)
print(eval(expression(1, 2, 3)))
