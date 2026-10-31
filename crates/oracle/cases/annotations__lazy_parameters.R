# A checked parameter stays a promise: it is evaluated when first used, or never.
log_lines <- character()
note <- function(text) {
  log_lines <<- c(log_lines, text)
  text
}
lazy <- function(x: double, y: double) -> double {
  note("body starts")
  if (missing(y)) x else x + y
}
lazy(note("x is forced") |> nchar() |> as.double())
log_lines
unused <- function(x: integer, never: character) -> integer x
unused(1L, stop("never evaluated"))
quoted <- function(x: double) -> character deparse(substitute(x))
quoted(a + b)
counter <- 0
side_effect <- function(x: double) -> double {
  x + x
}
side_effect({ counter <- counter + 1; 1 })
counter
cleanup <- function(x: double) -> double {
  on.exit(note("on.exit runs"))
  x * 2
}
cleanup(4)
tail(log_lines, 1L)
default_uses_other <- function(n: integer, width: integer = n * 2L) -> integer width
default_uses_other(3L)
handled <- tryCatch(stop("plain error"), error = function(e) conditionMessage(e))
handled
