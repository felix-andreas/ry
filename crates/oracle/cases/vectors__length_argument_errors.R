# `length<-` and `vector()` reject a bad length with different messages: the replacement says
# "invalid value" for a negative or missing length and names its `value` argument when it is not
# a scalar, while `vector()` and its wrappers say "invalid 'length' argument" for all three.
show <- function(expr) tryCatch(expr, error = function(e) list(conditionMessage(e), conditionCall(e)))
resize <- function(v) { x <- 1:3; length(x) <- v; x }
for (v in list(-1, NA, 1:2, 2, 5)) print(show(resize(v)))
for (n in list(-1, NA, 1:2, integer())) {
  print(show(vector("list", n))); print(show(character(n))); print(show(numeric(n)))
}
