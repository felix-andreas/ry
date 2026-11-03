# A `times` total past the longest vector is an invalid `times`, checked before anything is allocated.
show <- function(expr) print(tryCatch(expr, error = conditionMessage))
show(rep(1:3, times = c(2^63, 2^63, 2)))
show(rep.int(1:3, c(2^63, 2^63, 2)))
show(rep(1:2, times = 2^62))
show(rep.int(1:2, 2^62))
show(rep.int(1:2, c(-1, 2)))
show(rep.int(1:2, c(NA, 2)))
show(rep(c(a = 1, b = 2), times = c(2, 1)))
