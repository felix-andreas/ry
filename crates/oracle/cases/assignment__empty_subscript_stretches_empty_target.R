# An empty subscript longer than an empty vector still stretches it, so `x[FALSE] <- NULL` on
# `list()` leaves `list(NULL)`, while `NULL` given an empty value stays `NULL` for any index.
x <- 1:3; x[rep(FALSE, 5)] <- 1L; print(x)
x <- list(); x[FALSE] <- NULL; str(x)
x <- list(1); x[c(FALSE, FALSE, FALSE)] <- NULL; str(x)
x <- list(a = 1); x[c(FALSE, FALSE)] <- NULL; str(x)
x <- list(); x[FALSE] <- list(); str(x)
x <- list(); x[c(TRUE, FALSE)] <- NULL; str(x)
x <- list(); x[1] <- NULL; str(x)
x <- character(); x[FALSE] <- NULL; str(x)
x <- integer(); x[FALSE] <- character(); str(x)
x <- NULL; x[FALSE] <- 1; str(x)
x <- NULL; x[1] <- character(0); str(x)
x <- NULL; x[3] <- NULL; str(x)
x <- NULL; x[FALSE] <- list(); str(x)
r <- tryCatch({ x <- character(); x[1] <- NULL; x }, error = function(e) conditionMessage(e)); str(r)
x <- list(); x[FALSE] <- NULL; dim(x) <- c(1, length(x)); print(x)
f <- function() { x <- list(); x[FALSE] <- NULL; x }
for (i in 1:3) str(f())
