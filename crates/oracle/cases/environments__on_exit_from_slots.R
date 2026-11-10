# on.exit registered and run by frames that never need an environment.
ordered <- function() {
  x <- 1
  on.exit(cat("first", x, "\n"))
  on.exit(cat("second", x, "\n"), add = TRUE)
  on.exit(cat("before", x, "\n"), add = TRUE, after = FALSE)
  x <- 2
  invisible(x)
}
ordered()

replaced <- function() {
  on.exit(cat("dropped\n"))
  on.exit(cat("kept\n"))
  "value"
}
print(replaced())

assigns <- function() {
  x <- 1
  on.exit(x <- 10)
  on.exit(cat("x is", x, "\n"), add = TRUE)
  x
}
print(assigns())

close_it <- function(i) invisible(i)
listed <- function() {
  on.exit(close_it(1))
  on.exit(close_it(2), add = TRUE)
  sys.on.exit()
}
print(listed())
single <- function() {
  on.exit(close_it(1))
  sys.on.exit()
}
print(single())

on_error <- function() {
  on.exit(cat("cleanup after error\n"))
  stop("boom")
}
print(tryCatch(on_error(), error = conditionMessage))

returned_value <- function() {
  on.exit(return("from exit"))
  "from body"
}
print(returned_value())

caller_of_exit <- function() {
  on.exit(cat("exit sees", deparse(sys.call()), "\n"))
  1
}
invisible(caller_of_exit())

nested <- function(n) {
  on.exit(cat("leaving", n, "\n"))
  if (n > 0) nested(n - 1)
  n
}
invisible(nested(2))

with_promise <- function(arg) {
  on.exit(cat("arg was", arg, "\n"))
  arg * 2
}
print(with_promise(21))

visibility <- function() on.exit(NULL)
print(withVisible(visibility())$visible)
