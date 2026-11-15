# `a:b` evaluated many times inside one function, so a compiling engine builds it in compiled
# code, with the shapes whose results or errors only the builtin makes.
spans <- function(a, b, times = 300) {
  total <- 0
  for (i in seq_len(times)) {
    x <- a:b
    total <- total + length(x) + x[length(x)]
  }
  c(total, typeof(x))
}
print(spans(1L, 5L))
print(spans(5, 1))
print(spans(3L, 3L))
print(spans(TRUE, 4L))
print(spans(1, 2.5))
print(spans(0.5, 3))
print(spans(-2147483647, -2147483640))
print(tryCatch(spans(NA, 2), error = conditionMessage))
print(tryCatch(spans(1, NA_integer_), error = conditionMessage))
print(tryCatch(spans(integer(), 2), error = conditionMessage))

# A site that has made integers and then meets doubles.
mixed <- function(starts) {
  total <- 0
  for (start in starts) total <- total + sum(start:(start + 3))
  total
}
print(mixed(c(rep(1, 300), 0.5, 2)))

# Factors make their interaction, never a sequence.
interaction_levels <- function(f, g, times = 300) {
  for (i in seq_len(times)) x <- f:g
  x
}
print(interaction_levels(factor(c("a", "b")), factor(c("x", "y"))))

# A store into a variable that held a sequence, then reads of it in the same loop.
stores <- function(n) {
  x <- as.double(1:n)
  total <- 0
  for (j in seq_len(n)) {
    x[j] <- 0
    total <- total + x[n - j + 1]
  }
  c(total, sum(x))
}
print(stores(400))

# A callee rebinds the loop's sequence variable to another sequence.
rebind <- function() assign("x", as.double(2:9), envir = parent.frame())
rebinds <- function(times = 300) {
  total <- 0
  for (i in seq_len(times)) {
    x <- as.double(1:4)
    total <- total + x[2]
    if (i %% 2 == 0) rebind()
    total <- total + x[2]
  }
  total
}
print(rebinds())
