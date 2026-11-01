# Annotated code that satisfies its annotations behaves exactly like its erasure.
apply_discount <- function(price: double, rate: double = 0.2) -> double {
  price * (1 - rate)
}
apply_discount(100)
apply_discount(100L, rate = 0.5)
count: integer <- 3L
count
body(apply_discount)
names(formals(apply_discount))
formals(apply_discount)$rate
deparse(args(apply_discount))
label <- function(x: character | NULL = NULL) -> character {
  if (is.null(x)) "none" else x
}
c(label(), label("a"))
first <- function<T>(values: list<T>) -> T values[[1L]]
first(list(1, "b"))
totals <- function(...: double) -> double sum(...)
totals(1, 2L, TRUE)
person: list{name: character, age: integer} <- list(name = "Ada", age = 36L)
person$name
compose <- function(f: fn(double) -> double, g: fn(double) -> double) -> fn(double) -> double {
  function(x) f(g(x))
}
compose(sqrt, abs)(-16)
factorial_of <- function(n: integer) -> double if (n <= 1L) 1 else n * factorial_of(n - 1L)
factorial_of(10L)
identical(apply_discount, apply_discount)
copy <- apply_discount
environment(copy) <- new.env()
copy(10)
