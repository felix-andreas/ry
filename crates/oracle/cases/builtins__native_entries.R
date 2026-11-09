# Small base closures whose calls run natively when the arguments are values, and the
# one-argument builtins compiled code calls without dispatch. Each result must match the R body.

# identical(): values, promises of variables and of calls, attributes, flags passed by name.
x <- c(a = 1, b = 2)
y <- c(a = 1, b = 2)
identical(x, y)
identical(x, unname(y))
identical(NaN, NA_real_)
identical(0, -0)
identical(list(1, "a"), list(1, "a"))
identical(NULL, NULL)
identical(1L, 1)
identical(NaN, NA_real_, single.NA = FALSE)
identical(y = 1, x = 1)
f <- function(v) identical(v, 1)
f(1)
f({ cat("argument evaluated once\n"); 1 })
g <- function(v) { force(v); identical(v, v) }
g(2)
tryCatch(identical(1), error = conditionMessage)
tryCatch(identical(stop("in argument"), 1), error = conditionMessage)
depth <- function() length(sys.calls())
identical(depth(), depth())
identical(function(a) a, function(a) a)
identical(environment(), globalenv())

# isTRUE() and isFALSE(): scalars, NA, attributes, other types, a class with a length method.
isTRUE(TRUE)
isTRUE(c(TRUE, TRUE))
isTRUE(NA)
isTRUE(c(a = TRUE))
isTRUE(1)
isTRUE("TRUE")
isTRUE(NULL)
isTRUE(logical())
isFALSE(FALSE)
isFALSE(NA)
isFALSE(c(a = FALSE))
isFALSE(0)
flag <- structure(TRUE, class = "flag")
isTRUE(flag)
length.flag <- function(x) 2L
isTRUE(flag)
isFALSE(structure(FALSE, class = "flag"))
rm(length.flag)
isTRUE(matrix(TRUE))
h <- function(v) isTRUE(v)
h(TRUE)
h(NA)

# nrow() and ncol(): matrices, arrays, plain vectors, data frames and classes with dim methods.
m <- matrix(1:6, 2)
nrow(m)
ncol(m)
nrow(1:3)
ncol(1:3)
nrow(NULL)
a1 <- array(1:3, 3)
nrow(a1)
ncol(a1)
nrow(array(1:24, 2:4))
nrow(data.frame(u = 1:4, v = letters[1:4]))
ncol(data.frame(u = 1:4, v = letters[1:4]))
shaped <- structure(1:4, class = "shaped")
dim.shaped <- function(x) c(7L, 8L)
nrow(shaped)
ncol(shaped)
rm(dim.shaped)
nrow(structure(list(1, 2), dim = c(1L, 2L)))

# match() and %in%: strings, NA and "NA", encodings, numbers, factors, empty and NULL inputs.
match("b", c("a", "b", "c"))
match("z", c("a", "b"))
match(NA, c("a", NA))
match(NA_character_, c("a", NA))
match("NA", c("a", NA))
match("NA", c("NA", NA))
match(c("b", "z", NA), c("a", "b", NA))
latin <- iconv("café", "UTF-8", "latin1")
match(latin, c("x", "café"))
match("café", c("x", latin))
match(2L, c(1L, 2L, NA))
match(NA_integer_, c(1L, NA))
match(TRUE, c(FALSE, TRUE))
match(NA, c(TRUE, NA))
match(2, c(1, 2))
match(-0, 0)
match(NaN, c(NA, NaN))
match(NA_real_, c(NaN, NA))
match(2L, c(1, 2))
match(1:3, 2:4)
match("b", factor(c("a", "b")))
match(factor("b"), c("a", "b"))
match(list(1), list(1, 2))
match(NULL, 1:3)
match(1, NULL)
match(character(), "a")
match(5L, 1:10)
match(c(x = "a"), c(y = "a"))
"b" %in% c("a", "b")
"z" %in% c("a", "b")
NA %in% c(1, NA)
c(1, 5, NA) %in% c(1, 2)
1:3 %in% 2L
factor("a") %in% "a"
NULL %in% 1
list(1) %in% list(1)
`%in%`(table = c("a", "b"), x = "a")
tryCatch(match(sum, 1), error = conditionMessage)

# A traced closure runs its R body, so the trace prints.
trace("isTRUE", quote(cat("traced\n")), print = FALSE, where = baseenv())
isTRUE(TRUE)
untrace("isTRUE", where = baseenv())
isTRUE(TRUE)

# One-argument builtins that compiled code calls directly.
p <- function(v) list(tryCatch(!v, error = conditionMessage), length(v), is.null(v), is.na(v), is.character(v), is.logical(v), is.function(v))
str(p(TRUE))
str(p(NA))
str(p(c(a = TRUE)))
str(p(NULL))
str(p(NA_character_))
str(p("NA"))
str(p(NaN))
str(p(NA_integer_))
str(p(c(1, NA)))
str(p(list(1, NA)))
str(p(sum))
str(p(function() 1))
str(p(globalenv()))
str(p(1:10))
str(p(factor(c("a", NA))))
str(p(structure(TRUE, class = "flag")))
`!.flag` <- function(x) "negated flag"
length.flag <- function(x) 99L
is.na.flag <- function(x) "flag is.na"
str(p(structure(TRUE, class = "flag")))
rm(`!.flag`, length.flag, is.na.flag)
str(p(matrix(TRUE)))
q <- function(v) !v
tryCatch(q("a"), error = conditionMessage)
q(1)
q(0L)
r <- function(v) is.na(v)
r(c(a = 1))
r(NA_complex_)
r(pairlist(1, NA))

# A shared TRUE or FALSE is copied before it is modified.
k <- is.null(NULL)
k[2] <- FALSE
k
is.null(NULL)
n <- !TRUE
names(n) <- "x"
n
!TRUE
