speak <- function(x, ...) UseMethod("speak")
speak.default <- function(x, ...) "..."
speak.animal <- function(x, ...) paste("I am an animal named", x$name)
speak.dog <- function(x, ...) paste("Woof!", NextMethod())
d <- structure(list(name = "Rex"), class = c("dog", "animal"))
speak(d)
speak(1)
speak(structure(list(name = "Tom"), class = "animal"))
describe <- function(x) UseMethod("describe")
describe.numeric <- function(x) paste("numeric of length", length(x))
describe.integer <- function(x) paste("integer then", NextMethod())
describe(1:3)
describe(2.5)
describe.matrix <- function(x) "matrix"
describe(matrix(1:4, 2))
print.money <- function(x, ...) cat("$", format(unclass(x), nsmall = 2), "\n")
m <- structure(12.5, class = "money")
m
print(m)
format.money <- function(x, ...) paste0("$", unclass(x))
format(m)
toString.money <- function(x, ...) "money!"
toString(m)
length.stack <- function(x) length(unclass(x)$items)
s <- structure(list(items = 1:5), class = "stack")
length(s)

# With the first formal unsupplied, UseMethod dispatches on the first supplied argument.
first_missing <- function(x, y) UseMethod("first_missing")
first_missing.foo <- function(x, y) "foo"
first_missing.default <- function(x, y) "default"
first_missing(y = structure(1, class = "foo"))
first_missing()
