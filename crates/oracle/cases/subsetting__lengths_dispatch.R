# lengths() takes a classed object's elements from its `[[` method and their lengths from
# their `length` methods, so an object whose storage is not its elements still works.
x <- structure(list(), n = 3L, class = "virtual")
length.virtual <- function(x) attr(x, "n")
`[[.virtual` <- function(x, i) seq_len(i)
print(lengths(x))
y <- structure(list(a = 1:2, b = letters), class = "plain")
print(lengths(y))
z <- structure(list(1:3, 4), class = "short")
length.short <- function(x) 3L
print(tryCatch(lengths(z), error = function(e) "error"))
