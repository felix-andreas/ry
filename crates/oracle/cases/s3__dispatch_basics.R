speak <- function(x, ...) UseMethod("speak")
speak.default <- function(x, ...) "..."
speak.dog <- function(x, ...) paste("Woof", NextMethod())
speak.animal <- function(x, ...) "generic animal noise"
d <- structure(list(), class = c("dog", "animal"))
speak(d)
speak(1)
print.money <- function(x, ...) cat("$", format(unclass(x), nsmall = 2), "\n")
m <- structure(12.5, class = "money")
m
print(m)
format.money <- function(x, ...) paste0("$", unclass(x))
format(m)
toString.money <- function(x, ...) "money!"
toString(m)
inherits(d, "animal")
inherits(d, c("cat", "animal"), which = TRUE)
