# geterrmessage() holds the message of the last error a handler saw, calling or exiting.
show <- function() cat(deparse(geterrmessage()), "\n")
invisible(tryCatch(stop("plain"), error = function(condition) NULL))
show()
invisible(withCallingHandlers(
  tryCatch(stop("seen by both"), error = function(condition) NULL),
  error = function(condition) NULL
))
show()
f <- function() stop("from a call")
invisible(tryCatch(f(), error = function(condition) NULL))
show()
try(stop("in try"), silent = TRUE)
show()
invisible(tryCatch(warning("a warning"), warning = function(condition) NULL))
show()
