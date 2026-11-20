# Calls nested past options(expressions=) signal an expressionStackOverflowError without a call.
f <- function(n) f(n + 1)
condition <- tryCatch(f(1), error = function(e) e)
print(class(condition))
print(conditionMessage(condition))
print(conditionCall(condition))
print(inherits(tryCatch(f(1), stackOverflowError = function(e) e), "stackOverflowError"))
