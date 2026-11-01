# `**` is parsed as `^`.
print(2**3)
print(quote(2**3))
f <- function() 2.0**16.0 * 2
print(body(f))

# NextMethod reads `.Generic` before its `generic` argument.
xtfrm.foo <- function(x) NextMethod("misspelled")
x <- structure(c(3, 1, 2), class = "foo")
print(order(x))
predict.bar <- function(object, ...) NextMethod(object)
predict.default <- function(object, ...) "default reached"
print(predict(structure(list(1), class = "bar")))

# A length.out sequence of whole numbers in integer range is integer.
str(seq.int(0, by = 3600, length.out = 3))
str(seq.int(0.5, by = 1, length.out = 2))
str(seq.int(0, by = 0.5, length.out = 2))
str(seq.int(to = 5, by = 1, length.out = 2))
str(seq.int(to = 5.5, by = 1, length.out = 2))
str(seq.int(3e9, by = 1, length.out = 2))
str(seq.int(1, 10, length.out = 4))
str(seq(as.POSIXct("2020-08-13", tz = "UTC"), by = "hour", length.out = 3))

# enc2utf8 and enc2native accept only character vectors.
print(enc2utf8(c(a = "x")))
print(tryCatch(enc2utf8(logical()), error = conditionMessage))
print(tryCatch(enc2native(1), error = conditionMessage))
