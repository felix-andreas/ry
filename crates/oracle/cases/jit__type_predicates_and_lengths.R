# Compiled code answers a type predicate and `length` from the value's header where no method
# can apply, and calls the builtin for an object, an environment-like value or a lazily
# expanded vector. Every form agrees with the interpreter on plain values, classed values with
# and without methods, and unboxed scalars.

checks <- function(x) {
  c(
    null = is.null(x), logical = is.logical(x), integer = is.integer(x), double = is.double(x),
    numeric = is.numeric(x), complex = is.complex(x), character = is.character(x),
    raw = is.raw(x), list = is.list(x), pairlist = is.pairlist(x), fun = is.function(x),
    primitive = is.primitive(x), symbol = is.symbol(x), name = is.name(x), call = is.call(x),
    environment = is.environment(x), expression = is.expression(x), length = length(x)
  )
}
negated <- function(x) {
  if (!is.numeric(x) && !is.logical(x)) "neither" else if (length(x) != 1L) "several" else "one"
}
plain <- list(
  NULL, TRUE, c(TRUE, NA), 1L, 1:10, seq_len(0), 2.5, c(1, 2, 3), numeric(0), 1i, "a",
  c("a", "b"), character(0), as.raw(1:3), list(), list(1, "a"), pairlist(a = 1), sum, `if`,
  function(x) x, quote(name), quote(f(x)), globalenv(), expression(a, b + 1), 1:3 + 0.5,
  matrix(1:6, 2), array(1, c(2, 2, 2))
)
for (value in plain) checks(value)
for (value in plain) negated(value)
for (value in plain) print(checks(value))
print(vapply(plain, negated, ""))
print(checks(NULL))
print(negated(NULL))

# Classed values: a factor and a date are not numeric, a method answers where one exists, and
# a method's invisible result is visible, as a builtin's always is.
length.measured <- function(x) 99L
is.numeric.measured <- function(x) invisible(FALSE)
classed <- list(
  factor(c("a", "b", "a")), Sys.Date() - Sys.Date(), as.Date("2024-01-02"),
  structure(1:3, class = "measured"), structure(list(1, 2), class = "measured"),
  structure(1:3, class = "other"), structure(list(a = 1), class = "record_like"),
  data.frame(a = 1:2, b = c("x", "y")), as.POSIXct("2024-01-02", tz = "UTC"),
  numeric_version("1.2.3"), structure(2.5, class = "measured"), ordered(c("lo", "hi"))
)
for (value in classed) print(checks(value))
print(vapply(classed, negated, ""))
last_numeric <- function(x) is.numeric(x)
last_length <- function(x) length(x)
for (value in plain) last_numeric(value)
for (value in plain) last_length(value)
print(withVisible(last_numeric(structure(1:3, class = "measured")))$visible)
print(withVisible(last_length(structure(1:3, class = "measured")))$visible)
print(withVisible(last_numeric(1:3))$visible)
print(withVisible(is.numeric(structure(1:3, class = "measured")))$visible)
print(withVisible(length(structure(1:3, class = "measured")))$visible)
for (value in plain) print(checks(value))

# Scalars that compiled code holds unboxed.
scalars <- function(count, weight, flag) {
  total <- count + 1L
  share <- weight * 0.5
  both <- flag && count > 0L
  c(
    is.integer(total), is.numeric(total), is.double(total), is.character(total), length(total),
    is.integer(share), is.numeric(share), is.double(share), is.null(share), length(share),
    is.logical(both), is.numeric(both), is.list(both), length(both)
  )
}
for (i in 1:60) scalars(i, i / 2, TRUE)
print(scalars(3L, 2.5, FALSE))
print(scalars(NA_integer_, NA_real_, NA))

# Environments and long vectors.
keyed <- new.env()
assign("a", 1, envir = keyed)
assign("b", 2, envir = keyed)
print(last_length(keyed))
print(last_length(1:100000))
print(last_length(seq(0.5, 1000)))
print(last_length(as.character(1:5)))
print(last_length(quote(f(x, y))))
print(last_length(quote(name)))
print(last_length(sum))
