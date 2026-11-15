# A compiled body keeps the locals it assigns out of its frame, and writes them back at its
# return only when something reads the frame afterwards: exit code, a closure or a promise
# that outlives the call, or exit code a callee registered on the frame.

with_exit <- function(x) {
  total <- x * 2
  on.exit(cat("exit sees", total, "\n"))
  total <- total + 1
  total * 10
}
for (i in 1:3) print(with_exit(i))

counter <- function(start) {
  count <- start + 0
  bump <- function() count
  count <- count * 3 + 1
  bump
}
for (i in 1:3) print(counter(i)())

delayed <- function(x) {
  keep <- function(value) function() value
  scaled <- x * 5
  held <- keep(scaled + 1)
  scaled <- scaled - 100
  held
}
for (i in 1:3) print(delayed(i)())

defer_on_caller <- function(expression) {
  do.call(on.exit, list(substitute(expression), add = TRUE), envir = parent.frame())
}
deferred <- function(x) {
  label <- x + 0.5
  defer_on_caller(cat("deferred sees", label, "\n"))
  label <- label * 2
  label + 1
}
for (i in 1:3) print(deferred(i))

plain <- function(x, y) {
  a <- x * 2 + y
  b <- a * a - x
  word <- if (b > 50) "large" else "small"
  parts <- list(a, b)
  paste(word, (b - a) * (a + x), length(parts))
}
for (i in 1:5) print(plain(i, 3))

envs <- function(x) {
  first <- x + 1
  frame <- environment()
  second <- first * 2
  frame
}
for (i in 1:3) print(sort(ls(envs(i))))
print(get("second", envs(4)))
