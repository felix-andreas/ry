# `<<-` through a replacement into a locked binding fails without changing the value, and base's
# bindings are locked once start-up has run.
g <- c(1, 2); lockBinding("g", globalenv())
r <- tryCatch((function() g[1] <<- 5)(), error = function(e) conditionMessage(e)); print(r); print(g)
h <- function() tryCatch(g[2] <<- 7, error = function(e) conditionMessage(e))
for (i in 1:3) print(h())
print(g)
l <- list(a = 1); lockBinding("l", globalenv())
k <- function() l$a <<- 2
for (i in 1:2) print(tryCatch(k(), error = function(e) conditionMessage(e)))
print(l)
unlockBinding("g", globalenv()); (function() g[1] <<- 5)(); print(g)
r <- tryCatch((function() T[2] <<- FALSE)(), error = function(e) conditionMessage(e)); print(r)
print(exists("T", globalenv(), inherits = FALSE))
print(tryCatch((function() pi <<- 3)(), error = function(e) conditionMessage(e)))
print(tryCatch(assign("pi", 3, envir = baseenv()), error = function(e) conditionMessage(e)))
print(c(bindingIsLocked("T", baseenv()), bindingIsLocked("pi", baseenv()), bindingIsLocked("c", baseenv())))
print(c(bindingIsLocked(".Device", baseenv()), bindingIsLocked(".Devices", baseenv())))
# A local `T` is the user's own.
m <- function() { T <- c(TRUE, TRUE); T[2] <- FALSE; T }
print(m())
T <- 5; T[2] <- 6; print(T); rm(T); print(T)
op <- options(digits = 4); print(pi); options(op)
f <- function() warning("w"); f(); print(names(warnings()))
