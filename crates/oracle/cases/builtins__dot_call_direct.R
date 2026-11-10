# oracle: requires rlang
# `.Call` with positional arguments only, which ry calls without building an argument list:
# the condition call of an arity error, visibility,
# arguments that are promises or missing, and a routine called through a variable.
length_routine <- rlang:::ffi_length
err <- tryCatch(.Call(length_routine, 1, 2), error = identity)
print(conditionMessage(err))
print(conditionCall(err))
print(withVisible(.Call(length_routine, 1:4)))
lazy <- function(x) .Call(length_routine, x)
print(lazy(letters[1:3]))
r <- length_routine
loop <- function() { out <- integer(0); for (k in 1:5) out[k] <- .Call(r, seq_len(k)); out }
print(loop())
print(tryCatch(.Call(length_routine, ), error = conditionMessage))
print(tryCatch(.Call(), error = conditionMessage))
print(tryCatch(.Call(1, 2), error = conditionMessage))
