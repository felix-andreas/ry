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

# A matrix subscript drops rows holding a 0 and selects NA for rows holding an NA.
x <- matrix(1:6, ncol = 2)
k <- matrix(c(1, 1, 1, 0, 1, NA), ncol = 2, byrow = TRUE)
print(x[k])
print(tryCatch(x[matrix(c(-1, 1), nrow = 1)], error = conditionMessage))
print(tryCatch(x[-c(1, NA)], error = conditionMessage))
print(tryCatch(x[c(-1, 2)], error = conditionMessage))

# besselK of a high order stays finite where only the exponentially scaled value overflows.
print(besselK(100, c(450, 500, 540)))
print(besselK(100, 450, expon.scaled = TRUE))

# An array with named dimnames titles its rows, columns and slices.
d <- expand.grid(nu = 1:2, x = 3:4)
A <- array(1:8, c(2, 2, 2))
dimnames(A) <- c(attr(d, "out.attrs")$dimnames, list(c("p", "q")))
print(A)
print(array(1:8, c(2, 2, 2), dimnames = list(r = c("a", "b"), NULL, s = c("u", "v"))))

# rep.int and rep_len dispatch on their own name, then on the class's rep method; their own
# code keeps a factor a factor and drops every other attribute.
str(rep.int(as.factor(1L), 3))
str(rep_len(factor(c("a", "b")), 3))
str(rep.int(structure(1:2, names = c("a", "b"), foo = 1), 2))
str(rep.int(as.Date("2020-01-01"), 2))
str(rep_len(structure(c(a = 1, b = 2), class = "zzz"), 3))
rep.bar <- function(x, ...) "rep method"
rep_len.bar <- function(x, length.out) "rep_len method"
print(rep.int(structure(1, class = "bar"), 3))
print(rep_len(structure(1, class = "bar"), 2))

# An empty pattern matches an empty string under PCRE and fixed matching, but not under TRE.
print(unlist(gregexpr("", "", perl = TRUE)))
print(unlist(gregexpr("", "", fixed = TRUE)))
print(unlist(gregexpr("", "")))
print(regmatches(c("", "bar"), gregexpr("", c("", "bar"), perl = TRUE)))

# A cached S4 method inherited from a group generic still sees .Generic.
setClass("Measured", representation(x = "numeric"))
setMethod("Summary", "Measured", function(x, ..., na.rm = FALSE) {
  get(.Generic)(x@x, ..., na.rm = na.rm)
})
m <- new("Measured", x = c(1, 2, 3))
print(sum(m))
print(sum(m))
print(max(m, 10))

# The global environment does not keep the profile's .Library.site binding.
print(exists(".Library.site", envir = globalenv(), inherits = FALSE))
