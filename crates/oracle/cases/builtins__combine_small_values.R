# `c()`, `unlist()` and `do.call()` of small values, the conversions of a value that already has
# the type, and `nchar`. Each call runs several times from a function, so the first run of a
# site and its later ones are both covered.
suppressMessages(library(methods))
thrice <- function(f) for (i in 1:3) print(f())

cat("-- c() of tagged scalars\n")
thrice(function() c(a = 1, b = 2))
thrice(function() c(a = 1L, b = "x"))
thrice(function() c(a = 1L, 2:3, b = TRUE))
thrice(function() c(1, a = 2, NULL, 3))
thrice(function() c(a = NULL, 1))
thrice(function() c(a = NULL))
thrice(function() c(a = 1:2, b = 3))
thrice(function() c(a = c(x = 1), b = 2))
thrice(function() c(a = integer(0), b = 1))
thrice(function() c(`NA` = 1, b = NA))
thrice(function() c(a = list(1), b = 2))
thrice(function() c(a = factor("u"), b = 2))
thrice(function() c(a = as.Date("2024-01-02"), b = 2))
thrice(function() c(a = 1+2i, b = as.raw(1)))
thrice(function() c(a = TRUE, b = 2L, c = 3.5, d = "four"))
thrice(function() c(use.names = FALSE, a = 1, b = 2))
thrice(function() c(a = 1, b = 2, recursive = TRUE))
thrice(function() c(a = quote(x), b = 1))
thrice(function() names(c(a = 1, 2, b = 3)))
thrice(function() {
    first <- c(a = 1, b = 2)
    names(first)[1] <- "changed"
    first[["b"]] <- 20
    list(first, c(a = 1, b = 2))
})

cat("-- c() of untagged values through a call with evaluated arguments\n")
thrice(function() do.call(c, list(1L, "a", TRUE)))
thrice(function() Reduce(c, list(1, 2:3, NULL, 4.5)))
thrice(function() (function(...) c(...))(1L, 2L, 3L))
thrice(function() (function(...) c(...))(list(1), "a"))
thrice(function() (function(...) c(...))(NULL, NULL))

cat("-- unlist() of a named list of scalars\n")
named <- list(a = 1, b = 2, c = 3)
thrice(function() unlist(named))
thrice(function() unlist(list(a = 1L, b = "x")))
thrice(function() unlist(list(a = 1, 2, b = 3)))
thrice(function() unlist(setNames(list(1, 2), c("a", NA))))
thrice(function() unlist(setNames(list(1, 2), c("", ""))))
thrice(function() unlist(named, use.names = FALSE))
thrice(function() unlist(list(a = 1, b = NULL, c = 3)))
thrice(function() unlist(list(a = 1:2, b = 3)))
thrice(function() unlist(list(a = c(x = 1), b = 3)))
thrice(function() unlist(list(a = list(1), b = 3)))
thrice(function() unlist(list(a = factor("u"), b = factor("v"))))
thrice(function() unlist(setNames(list(), character())))
thrice(function() unlist(list(a = integer(0), b = NULL)))
thrice(function() unlist(list(a = TRUE, b = 2L, c = 3.5, d = 1i)))
thrice(function() unlist(pairlist(a = 1, b = 2)))
thrice(function() unlist(list(a = quote(x), b = 1)))
thrice(function() {
    values <- unlist(named)
    names(values)[2] <- "changed"
    list(values, names(named), unlist(named))
})
noted <- named
attr(noted, "names") <- structure(c("p", "q", "r"), note = "on the names")
thrice(function() attributes(unlist(noted)))
boxed <- array(list(1, 2), 2, list(c("row1", "row2")))
thrice(function() unlist(boxed))

cat("-- do.call() of c from a function\n")
ten <- as.list(as.numeric(1:10))
thrice(function() do.call("c", ten))
thrice(function() do.call("c", named))
thrice(function() do.call("c", list(a = 1, 2:3)))
thrice(function() do.call("c", list(as.Date("2024-01-02"), as.Date("2024-03-04"))))
thrice(function() do.call("c", list(1, as.Date("2024-01-02"))))
thrice(function() do.call("c", list(factor("u"), factor("v"))))
thrice(function() do.call("c", list(list(1), 2)))
thrice(function() do.call("c", list(NULL, NULL)))
thrice(function() do.call("c", list()))
c.widget <- function(...) "the global method"
widget <- structure(list(1), class = "widget")
thrice(function() do.call("c", list(widget, 1)))
thrice(function() do.call("c", list(1, widget)))
thrice(function() {
    c.gadget <- function(...) "the caller's own method"
    do.call("c", list(structure(1, class = "gadget"), 2))
})
setClass("Item", representation(value = "numeric"))
setMethod("c", "Item", function(x, ...) "the S4 method")
item <- new("Item", value = 1)
thrice(function() do.call("c", list(item, 1)))
thrice(function() do.call("c", list(1, item)))

cat("-- do.call() of other builtins that dispatch\n")
thrice(function() do.call("length", list(1:3)))
thrice(function() do.call("+", list(1, 2)))
thrice(function() do.call("sum", list(1, 2, NA, na.rm = TRUE)))
thrice(function() do.call("[", list(letters, 2:3)))
thrice(function() do.call("[[", list(named, "b")))
thrice(function() do.call("as.character", list(1:2)))
thrice(function() do.call("+", list(as.Date("2024-01-02"), 1)))
thrice(function() do.call("max", list(as.Date("2024-01-02"), as.Date("2024-03-04"))))
thrice(function() do.call("format", list(as.Date("2024-01-02"))))
length.widget <- function(x) 99L
thrice(function() do.call("length", list(widget)))
thrice(function() {
    length.gadget <- function(x) 7L
    do.call("length", list(structure(1, class = "gadget")))
})
thrice(function() {
    Ops.gadget <- function(e1, e2) paste("the caller's own", .Generic)
    do.call("+", list(1, structure(1, class = "gadget")))
})
thrice(function() tryCatch(do.call("+", list("a", 1)), error = conditionMessage))

cat("-- a conversion of a value that already has the type\n")
conversions <- function(value) {
    list(as.integer(value), as.double(value), as.numeric(value), as.character(value), as.logical(value))
}
subjects <- list(
    1L, 2.5, "3", TRUE, c(a = 1L), c(a = 1.5), c(a = "x"), c(a = TRUE), factor("u"),
    matrix(1:4, 2), 1:3, structure(2L, class = "gadget"), structure("t", note = "kept?")
)
for (subject in subjects) for (i in 1:2) str(conversions(subject))
as.character.widget <- function(x, ...) "the widget's method"
as.integer.widget <- function(x, ...) 42L
tagged <- structure("w", class = "widget")
for (i in 1:3) print(list(as.character(tagged), as.integer(tagged)))
thrice(function() {
    original <- c(1L, 2L)
    converted <- as.integer(original)
    converted[1] <- 9L
    list(original, converted)
})
thrice(function() {
    original <- c("a", "b")
    converted <- as.character(original)
    converted[2] <- "changed"
    list(original, converted)
})
thrice(function() withVisible(as.numeric(2.5))$visible)
thrice(function() as.integer())
thrice(function() tryCatch(as.integer(1L, 2L), error = conditionMessage))

cat("-- nchar\n")
texts <- c("alpha", "", "été", NA, "日本", "a b")
thrice(function() nchar(texts))
thrice(function() nchar(texts, "bytes"))
thrice(function() nchar(texts, "width"))
thrice(function() nchar(texts, keepNA = FALSE))
thrice(function() nchar(c(first = "xy", second = "xyz")))
thrice(function() nchar(matrix(c("a", "bb", "ccc", ""), 2)))
thrice(function() nchar(123456))
thrice(function() nchar(c(TRUE, NA)))
latin <- iconv("été", "UTF-8", "latin1")
thrice(function() c(nchar(latin), nchar(latin, "bytes")))
thrice(function() tryCatch(nchar(factor("u")), error = conditionMessage))
