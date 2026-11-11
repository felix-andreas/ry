# A single value skips NA rows and columns in a matrix or array assignment; a longer value
# is an error, because it cannot say which of its elements an NA row would have taken.
m <- matrix(1:12, 3)
m[NA, 1] <- 0L
print(m)
m[c(1, NA), 2] <- 0L
print(m)
print(tryCatch({ m[c(1, NA), 2] <- c(5L, 6L); m }, error = conditionMessage))
m[c(1, NA), 2:3] <- 7L
print(m)
print(tryCatch({ m[c(1, NA), 2:3] <- 1:4; m }, error = conditionMessage))
print(tryCatch({ m[c(1, NA), 2:3] <- 1:3; m }, error = conditionMessage))
m[NA_integer_, NA] <- 9L
print(m)
a <- array(1:24, 2:4)
a[NA_integer_, 1, 2] <- 0L
print(a[, , 2])
a[1:2, c(NA, 3), 4] <- -1L
print(a[, , 4])
l <- matrix(list(1, 2, 3, 4), 2)
l[c(NA, 2), 1] <- list("x")
print(l)
print(tryCatch({ m[5, 1] <- 1L }, error = conditionMessage))
# The order of the checks: a matrix rejects NA before comparing lengths, an array after.
print(tryCatch({ m[c(1, NA), 2] <- integer(0); "ok" }, error = conditionMessage))
print(tryCatch({ m[integer(0), NA] <- 1:3; "ok" }, error = conditionMessage))
print(tryCatch({ m[integer(0), 1] <- integer(0); "ok" }, error = conditionMessage))
print(tryCatch({ a[c(1, NA), 1, 1:3] <- 1:4; "ok" }, error = conditionMessage))
print(tryCatch({ a[c(1, NA), 1, 1] <- 1:2; "ok" }, error = conditionMessage))
print(tryCatch({ a[integer(0), NA, 1] <- 1:3; "ok" }, error = conditionMessage))
print(tryCatch({ a[c(1, NA), 1, 1] <- integer(0); "ok" }, error = conditionMessage))
