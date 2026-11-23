# A `for` loop of compiled code steps through any vector tier 0 steps through: strings, a
# vector with names or a class, a factor (its labels), a list, and a sequence that changes
# its type between calls.

widths <- function(xs) {
  total <- 0L
  for (x in xs) if (nchar(x) > 1L) total <- total + nchar(x)
  total
}
joined <- function(xs) {
  out <- ""
  for (x in xs) out <- paste0(out, "<", x, ">")
  out
}
classes <- function(xs) {
  out <- character(0)
  for (x in xs) out <- c(out, class(x)[1L])
  out
}
last <- function(xs) {
  seen <- NULL
  for (x in xs) seen <- x
  seen
}

tags <- c("t1", "t22", "t333")
named <- c(a = 1.5, b = 2.5, c = 4)
days <- as.Date("2024-01-02") + 0:2
level <- factor(c("high", "low", "high"), levels = c("low", "high"))
pieces <- list(1L, "two", c(3, 4), NULL, list(5))
stamp <- as.POSIXct("2024-01-02 03:04:05", tz = "UTC") + c(0, 60)

for (k in 1:40) {
  widths(tags); joined(tags); classes(tags); last(tags)
}
print(widths(tags))
print(joined(tags))
print(joined(named))
print(joined(days))
print(joined(level))
print(joined(1:3))
print(joined(c(TRUE, NA)))
print(joined(pieces[1:2]))
print(classes(tags))
print(classes(named))
print(classes(days))
print(classes(level))
print(classes(pieces))
print(classes(stamp))
print(classes(seq_len(3)))
print(classes(c(2i, 1)))
print(classes(as.raw(1:2)))
print(last(named))
print(last(days))
print(last(level))
print(last(pieces))
print(last(character(0)))
print(last(NULL))
print(widths(character(0)))
print(tryCatch(widths(quote(f(x))), error = function(e) conditionMessage(e)))
print(tryCatch(widths(globalenv()), error = function(e) conditionMessage(e)))
print(tryCatch(last(sum), error = function(e) conditionMessage(e)))
print(last(pairlist(1, "b")))
print(last(expression(a, b + 1)))
for (k in 1:40) {
  joined(tags); joined(named); joined(level); joined(pieces[1:2]); joined(NULL)
}
print(joined(tags))
print(joined(level))

# The loop's sequence is fixed at its start, whatever the body assigns.
growing <- function(xs) {
  count <- 0L
  for (x in xs) {
    xs <- c(xs, x)
    count <- count + 1L
  }
  c(count, length(xs))
}
for (k in 1:40) growing(tags)
print(growing(tags))
print(growing(level))
