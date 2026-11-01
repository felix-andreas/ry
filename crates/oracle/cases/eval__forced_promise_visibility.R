f <- function() invisible(1)
withCallingHandlers(f(), message = function(m) NULL)
suppressMessages(f())
g <- function(x) x
g(invisible(2))
h <- function() 3
withCallingHandlers(h(), message = function(m) NULL)
