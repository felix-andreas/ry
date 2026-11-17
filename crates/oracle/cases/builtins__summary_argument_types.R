# The Summary group's argument types: `sum` and `prod` take complex but not character, `min`
# and `max` the reverse, none takes raw or a list, and `range` combines its arguments first.
probe <- function(code) {
  cat(code, "-> ")
  tryCatch(
    str(withCallingHandlers(eval(parse(text = code)), warning = function(w) {
      cat("warning:", conditionMessage(w), "| ")
      invokeRestart("muffleWarning")
    })),
    error = function(e) cat("error:", conditionMessage(e), "\n")
  )
}
r <- as.raw(1:18)
for (code in c(
  "sum(r)", "sum(raw())", "sum(r, 1L)", "sum(1L, r)", "sum(1, r)", "sum(r, \"a\")", "sum(\"a\", r)",
  "prod(r)", "prod(raw())", "prod(1i, r)", "prod(character())",
  "max(r)", "min(r)", "max(1, r)", "min(\"a\", r)", "max(raw())",
  "max(1i)", "min(complex())", "max(1L, 1i)", "max(\"a\", 1i)", "max(list(1))",
  "max(character())", "min(character())", "max(NA_character_, na.rm = TRUE)", "min(1, character())",
  "range(r)", "range(1i)", "range(complex())", "range(character())", "range(TRUE, \"a\")",
  "range(1L, as.raw(2))", "range(as.raw(2), 1)", "range(as.raw(2), logical())", "range(as.raw(2), NULL)",
  "range(as.raw(2), factor(\"x\"))", "range(list(1, 2))", "range(c(\"b\", NA, \"a\"), finite = TRUE)",
  "mean(r)", "mean(\"a\")"
)) probe(code)
f <- function(x) sum(x)
for (i in 1:3) probe("f(r)")
g <- function(x, y) max(x, y)
for (i in 1:3) probe("g(1, r)")
for (i in 1:3) probe("g(1i, 2)")
h <- function(x) range(x)
for (i in 1:3) probe("h(r)")
