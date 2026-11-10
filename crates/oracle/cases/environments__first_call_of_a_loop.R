# A function whose body has a loop compiles at its first call, before its arguments are bound,
# so even that call runs in a slot frame. Each call here is the function's first.
count <- function(n, by = 1) {
  total <- 0
  for (i in seq_len(n)) total <- total + by
  c(total = total, missing_by = missing(by))
}
print(count(3))
print(eval(quote(count(4, by = 2))))

observe <- function(n) {
  i <- 0
  while (i < n) i <- i + 1
  list(call = sys.call(), local = ls(environment()), parent = identical(parent.frame(), globalenv()))
}
str(observe(2))

late <- function(x) {
  repeat break
  x
}
value <- 1
print(capture.output(print(late(value + 1))))

nested <- function(n) {
  inner <- function() n * 2
  for (k in 1:2) n <- n + 1
  inner()
}
print(nested(1))
