# An error raised by compiled code and caught by an exiting handler: the handler's result travels
# back to its `tryCatch` while compiled code deoptimizes, which allocates. Found by
# `oracle fuzz --seed 9278`.
f <- function() 0L + list(1)
for (r in 1:4) print(tryCatch(f(), error = function(e) conditionMessage(e)))
for (r in 1:4) print(tryCatch(f(), error = function(e) NULL))
for (r in 1:4) try(f(), silent = TRUE)
print(inherits(try(f(), silent = TRUE), "try-error"))
