# Compiled code that leaves for the interpreter on most of its calls is discarded, and the body
# then runs in the interpreter. The result of every call is the same before the code is
# compiled, while it runs, as it leaves, and after it is discarded.

parse_number <- function(text) {
  value <- suppressWarnings(as.numeric(text))
  if (is.na(value)) stop("not a number: ", text)
  value
}
attempt <- function(text) tryCatch(parse_number(text), error = function(e) -1)
for (i in 1:30) parse_number("1")
results <- numeric()
for (round in 1:400) results[[round]] <- attempt(if (round %% 3L == 0L) "12" else "x")
print(table(results))
for (round in 1:400) results[[round]] <- attempt("x")
print(table(results))
for (round in 1:400) results[[round]] <- attempt(as.character(round))
print(sum(results))

kind <- function(x) {
  switch(class(x)[1L],
    numeric = return("a number"),
    character = return("text"),
    logical = {
      if (isTRUE(x)) return("yes")
      "a flag"
    }
  )
}
for (i in 1:30) kind(i)
kinds <- character()
inputs <- list(1, "a", TRUE, FALSE, NULL, 2L)
for (round in 1:600) kinds[[round]] <- paste0("", kind(inputs[[round %% 6L + 1L]]))
print(table(kinds))

# A body whose calls are handed to the interpreter one after another, alone and inside a call
# that runs without a frame, with native arithmetic between them.
shout <- function(text, ...) toupper(text)
join <- function(left, right, ...) paste0(left, "-", right)
build <- function(a, b) {
  first <- shout(a)
  n <- nchar(first) + 1L
  second <- shout(b)
  m <- n * 2L
  third <- join(first, second)
  paste(third, n, m, nchar(third))
}
for (i in 1:30) { shout("a"); join("a", "b") }
for (round in 1:40) build("ab", "cde")
print(build("ab", "cde"))
print(build("", NA))
outer <- function(a, b, times) {
  out <- character()
  for (i in seq_len(times)) out[[i]] <- build(a, paste0(b, i))
  out
}
for (round in 1:40) outer("k", "v", 3L)
print(outer("key", "value", 4L))

# The same call fails on some rounds and returns on others, under a handler that resumes.
flaky <- function(x) {
  if (x %% 2L == 0L) warning("even")
  x * 3L
}
for (i in c(1L, 3L, 5L)) flaky(i)
resume <- function(x) withCallingHandlers(flaky(x), warning = function(w) invokeRestart("muffleWarning"))
total <- 0L
for (round in 1:600) total <- total + resume(round)
print(total)
print(tryCatch(flaky(2L), warning = function(w) conditionMessage(w)))
print(flaky(3L))

# One instruction keeps leaving while the rest of the body runs compiled: a subscript that
# selects with a negative index after reads of one element, a read past the end, and an
# integer sum that overflows. Each is compiled as the interpreter's own step afterwards.
rewrite <- function(e) {
  if (is.call(e) && !is.function(e[[1L]])) {
    if (e[[1L]] == ".") e[[1L]] <- quote(list)
    for (i in seq_along(e)[-1L]) if (!is.null(e[[i]])) e[[i]] <- rewrite(e[[i]])
  }
  e
}
expression <- quote(.(key, total = sum(value), .(nested, .(deep))))
for (round in 1:400) rewritten <- rewrite(expression)
print(rewritten)
pick <- function(x, i) {
  value <- x[i]
  n <- 0L
  for (k in 1:2) n <- n + k
  value + n
}
for (round in 1:30) pick(c(3L, 5L, 8L), 2L)
picked <- integer()
for (round in 1:400) picked[[round]] <- pick(c(3L, 5L, 8L), if (round %% 2L == 0L) 9L else -1L)[1L]
print(table(picked, useNA = "always"))
print(pick(c(3L, 5L, 8L), 3L))
add <- function(a, b) {
  total <- a + b
  n <- 0L
  for (k in 1:2) n <- n + k
  total + n
}
for (round in 1:30) add(1L, 2L)
warned <- 0L
for (round in 1:400) {
  withCallingHandlers(
    add(.Machine$integer.max, round),
    warning = function(w) {
      warned <<- warned + 1L
      invokeRestart("muffleWarning")
    }
  )
}
print(warned)
print(add(3L, 4L))
empty <- function(values) {
  total <- 0L
  for (value in values) total <- total + value
  total
}
for (round in 1:30) empty(1:3)
for (round in 1:400) empty(integer())
print(empty(integer()))
print(empty(1:4))
