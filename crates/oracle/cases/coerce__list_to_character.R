# as.character of a list, pairlist or expression: a string scalar is taken as it is, and every
# other element is deparsed on one logical line without the integer suffix or a typed NA.
xs <- list(1:3, c(a = 1L), 2L, c(1.5, 2), c(TRUE, NA), c("s", NA), NULL, quote(x + y), list(1L, 2L), c(a = 1L, b = NA),
  structure(1:2, class = "foo"), c(NA_integer_, 2L), c(NA_real_, 1), factor(c("u", "v")), function(x) x + 1L, list(a = 1L, 2.5),
  NA_integer_, c(a = NA_character_), as.Date("2024-01-02"), c(1i, 2), as.raw(1:2), quote(f(1L, 2L)), 1e6L, c(100000L, 5L),
  NA, NA_real_, NA_character_, NA_complex_, as.raw(1), 1e5, 1/3, 1e-20, 123456789012, TRUE, 3+2i, c(a = NA), c(a = "s"), c(a = 2.5),
  structure(2L, class = "bar"), structure(2.5, note = "n"), structure("s", class = "baz"), function(x) { x; 2L }, quote({a; b}),
  seq(0.5, 80, by = 0.5), quote(if (a) b else c), as.name("a b"), list(), character(0), integer(0), -1L, -2.5, Inf, -Inf, NaN,
  c(NA_integer_, NA_integer_), c(a = NA_integer_, b = NA_integer_), list(x = NULL, 3L), 100000L, 1e15, 0.1 + 0.2, c(x = TRUE),
  matrix(1:4, 2), "plain", c("a", "b"), quote(x), 5i, expression(a, 1L), globalenv, c(NA_real_, NA_real_), c(1L, NA), c(1.5, NA))
x <- as.character(xs)
print(x)
print(is.na(x))
print(nchar(x))
print(as.character(pairlist(c(1L, 2L), a = 3L, NA, quote(g(1L)))))
print(as.vector(xs[c(2, 9, 25, 42)], "character"))
print(as.character(expression(a + 1L, c(1L, 2L), 3L, NA, function(x) 1L)))
print(sapply(xs[c(2, 9, 25)], as.character))
print(deparse(as.raw(1)))
print(deparse(as.raw(1:2)))
print(deparse(structure(as.raw(1), names = "a")))
print(deparse(as.raw(0:60)))
print(deparse(list(as.raw(1), raw(0))))
