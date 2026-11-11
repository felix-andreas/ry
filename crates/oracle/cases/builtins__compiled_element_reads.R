# x[i] and x[[i]] inside compiled closures, where a plain vector and a plain numeric
# position read the element without the call, and everything else takes the builtin.

read2 <- function(x, i) x[[i]]
read1 <- function(x, i) x[i]
for (round in 1:3) {
  print(read2(c(1.5, 2.5, 3.5), 2L))
  print(read2(c(1.5, 2.5, 3.5), 2.9))
  print(read2(4:6, 3))
  print(read2(c(TRUE, NA, FALSE), 2L))
  print(read2(list(1, "b", NULL), 2L))
  print(read2(list(1, "b", NULL), 3L))
  print(read2(c(a = 1, b = 2), 2L))
  print(read2(list(a = 1, b = list(2)), 2L))
  print(read2(c("x", "y"), 1L))
  print(read2(1:10, 4L))
  print(read2(matrix(1:6, 2), 5L))
  print(read2(factor(c("u", "v")), 2L))
  print(read2(c(1, 2), c(a = 2L)))
  print(read2(c(1, 2), TRUE))
  print(read2(list(a = 1, b = 2), "b"))
  print(tryCatch(read2(c(1, 2), 3L), error = conditionMessage))
  print(tryCatch(read2(c(1, 2), 0L), error = conditionMessage))
  print(tryCatch(read2(c(1, 2), -1L), error = conditionMessage))
  print(tryCatch(read2(c(1, 2), NA_integer_), error = conditionMessage))
  print(tryCatch(read2(c(1, 2), NaN), error = conditionMessage))
  print(tryCatch(read2(list(1), 2L), error = conditionMessage))
  print(tryCatch(read2(c(1, 2), c(1L, 2L)), error = conditionMessage))
  print(read1(c(1.5, 2.5, 3.5), 2L))
  print(read1(c(1.5, 2.5, 3.5), 3.7))
  print(read1(4:6, 1))
  print(read1(c(FALSE, TRUE), 2L))
  print(read1(c(a = 1, b = 2), 2L))
  print(read1(list(1, 2), 2L))
  print(read1(c(1, 2), 3L))
  print(read1(c(1, 2), 0L))
  print(read1(c(1, 2), -1L))
  print(read1(c(1, 2), NA_integer_))
  print(read1(structure(1:3, class = "myclass"), 2L))
  print(read1(matrix(1:6, 2), 4L))
}

# The element is a value of its own: changing it leaves the vector alone.
own <- function() {
  x <- c(1, 2, 3)
  l <- list(c(1, 2), 3)
  for (k in 1:2) {
    y <- x[[k]]
    y[1] <- 99
    e <- l[[1]]
    e[1] <- 99
    z <- x[k]
    z[1] <- -1
  }
  list(x, l, y, e, z)
}
print(own())
print(own())

# A loop whose subject changes type part way through.
mixed <- function(values) {
  out <- list()
  for (k in seq_along(values)) out[[k]] <- values[[k]][[1L]]
  out
}
print(mixed(list(c(1, 2), 3:4, list("a"), c(x = TRUE), factor("f"), "s")))
print(mixed(list(c(1, 2), 3:4, list("a"), c(x = TRUE), factor("f"), "s")))

# Methods still see `[[` and `[` on classed subjects.
`[[.box` <- function(x, i) paste("box item", i)
`[.box` <- function(x, i) paste("box subset", i)
boxed <- function(b) for (k in 1:2) print(c(b[[k]], b[k]))
boxed(structure(list(1, 2), class = "box"))
boxed(structure(list(1, 2), class = "box"))

# Function lookups that the closure cached: a later non-function binding is skipped, a later
# function in front of the cached one is found, and a cached function rebound to a value is no
# longer a function.
helper <- function() "helper"
calls <- function() c(helper(), paste("p", 1))
print(calls())
print(calls())
paste <- 5
print(calls())
paste <- function(...) "masked paste"
print(calls())
rm(paste)
print(calls())
helper <- 2
print(tryCatch(calls(), error = conditionMessage))

# A one-string index reads the first element with exactly that name.
by_name <- function(x, key) {
  out <- list()
  for (k in 1:2) out[[k]] <- tryCatch(x[[key]], error = conditionMessage)
  out
}
str(by_name(list(a = 1, b = "x", b = 3), "b"))
str(by_name(c(a = 1, b = 2), "b"))
str(by_name(c(a = 1L, b = 2L), "a"))
str(by_name(c(a = TRUE), "a"))
str(by_name(list(a = 1), "z"))
str(by_name(c(a = 1), "z"))
str(by_name(list(1, 2), "a"))
str(by_name(list(ab = 1), "a"))
str(by_name(list("é" = 1), "é"))
str(by_name(list(a = 1), NA_character_))
str(by_name(list(a = 1), ""))
str(by_name(list(a = 1), c(k = "a")))

name_stores <- function() {
  v <- c(a = 1, b = 2)
  l <- list(a = 1, b = 2)
  for (k in 1:2) {
    v[["b"]] <- k
    v["a"] <- -k
    l[["a"]] <- "s"
    l["b"] <- list(k)
  }
  v[["c"]] <- 3
  list(v, l)
}
print(name_stores())
print(name_stores())

na_read <- function(x) {
  out <- list()
  for (k in 1:2) out[[k]] <- tryCatch(x[[NA_character_]], error = conditionMessage)
  out
}
with_na <- list(a = 1, 2)
names(with_na)[2] <- NA
str(na_read(with_na))
with_na_dbl <- c(a = 1, 2)
names(with_na_dbl)[2] <- NA
str(na_read(with_na_dbl))
