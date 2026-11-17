# seq_len() of an argument that is not of length one warns "first element used of 'length.out'
# argument" and goes on with its first element; an empty argument then fails as not coercible
# to a non-negative integer, so `seq_len(nrow(x))` on a plain vector warns before it errors.
show <- function(expr) withCallingHandlers(
  tryCatch(expr, error = function(e) list(error = conditionMessage(e), call = conditionCall(e))),
  warning = function(w) { print(list(warning = conditionMessage(w), call = conditionCall(w))); invokeRestart("muffleWarning") })
print(show(seq_len(NULL)))
print(show(seq_len(integer(0))))
print(show(seq_len(character(0))))
print(show(seq_len(1:3)))
print(show(seq_len(c(2.5, 1))))
print(show(seq_len("3")))
print(show(seq_len(list(2))))
print(show(seq_len(NA)))
print(show(seq_len(-1)))
x <- as.Date("2024-02-29") + 0:3
print(show(seq_len(nrow(x))))
f <- function(x) seq_len(nrow(x))
print(show(f(1:5)))
print(show(f(data.frame(a = 1:2))))
