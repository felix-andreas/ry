# Compiled code runs a closure call without a frame when the arguments are constants of any
# type: a string, NULL, a logical, a constant vector. The callee treats such an argument like
# any value: changing it changes a copy, never the constant the next call passes. A callee the
# interpreter ran without recording what its instructions saw is compiled from its
# instructions alone, and a late instruction of it that needs the frame gets one.

describe <- function(value, label, fallback) {
  if (is.null(value)) return(fallback)
  paste(label, length(value))
}
for (i in 1:30) describe(i, "n", "none")
call_describe <- function(x) {
  list(describe(x, "given", "nothing"), describe(NULL, "null", "nothing"),
       describe("text", "string", NULL), describe(TRUE, "flag", NA))
}
for (round in 1:40) call_describe(round)
print(call_describe(1:3))
print(call_describe(NULL))

grow <- function(values, extra) {
  values[[length(values) + 1L]] <- extra
  values
}
for (i in 1:30) grow(1, i)
grown <- function(extra) grow(c(1, 2, 3), extra)
for (round in 1:40) grown(round)
print(grown(10))
print(grown(20))
print(grown("text"))
print(grown(30))

rename <- function(labels, at, to) {
  labels[at] <- to
  labels
}
for (i in 1:30) rename(c("a", "b"), 1L, "z")
renamed <- function(at, to) rename(c("x", "y", "z"), at, to)
for (round in 1:40) renamed(1L, "first")
print(renamed(2L, "second"))
print(renamed(3L, "third"))
print(renamed(1L, NA))

tag <- function(value, kind = "plain", extra = NULL) {
  attr(value, "kind") <- kind
  if (!is.null(extra)) attr(value, "extra") <- extra
  value
}
for (i in 1:30) tag(i)
tagged <- function(flag) {
  if (flag) tag("constant", "special", c(a = 1)) else tag("constant")
}
for (round in 1:40) tagged(FALSE)
print(tagged(FALSE))
print(tagged(TRUE))
print(tagged(FALSE))

# The callees below are one expression each, so the interpreter runs them without an
# activation of their own and records nothing about them.
field <- function(x, name) x[[name]]
has <- function(x, name) !is.null(x[[name]])
default <- function(x, otherwise) if (is.null(x)) otherwise else x
record <- list(id = 7L, label = "seven", weights = c(0.5, 0.25))
lookup <- function(record) {
  list(field(record, "id"), has(record, "label"), has(record, "missing"),
       default(field(record, "missing"), "absent"), default(field(record, "weights"), 0))
}
for (round in 1:40) lookup(record)
print(lookup(record))
print(lookup(list(id = "text", missing = FALSE)))
print(tryCatch(lookup(NULL), error = function(e) conditionMessage(e)))
print(tryCatch(lookup(7), error = function(e) conditionMessage(e)))
print(lookup(record))

where <- function(x, ask) if (ask) sys.call() else x
whose <- function(x, ask) if (ask) environmentName(parent.frame(2L)) else x
count <- function(x, ask, ...) if (ask) nargs() else x
asks <- function(x, ask) list(where(x, ask), whose(x, ask), count(x, ask))
for (round in 1:40) asks(round, FALSE)
print(asks(1, FALSE))
print(asks(2, TRUE))
print(asks(3, FALSE))
for (round in 1:40) asks(round, TRUE)
print(asks(4, TRUE))
