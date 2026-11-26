# A replacement call or a matrix element store on an argument changes the function's own copy.
# An argument forwarded through `...` is the caller's value, so `..1` still reads it unchanged.
# Each function runs three times so that its compiled body runs as well.
rename <- function(x) {
  names(x) <- c("p", "q")
  x
}
store <- function(m) {
  m[1, 1] <- 99
  m
}
forward_rename <- function(...) {
  inner <- rename(...)
  list(names(inner), names(..1))
}
forward_store <- function(...) {
  inner <- store(...)
  c(inner[1, 1], ..1[1, 1])
}
a <- c(1, 2)
for (i in 1:3) {
  r <- rename(a)
  cat(names(r), is.null(names(a)), "\n")
  cat(names(rename(c(a = 1, b = 2))), "\n")
  forwarded <- forward_rename(c(a = 1, b = 2))
  cat(forwarded[[1]], forwarded[[2]], "\n")
  m <- matrix(0, 2, 2)
  cat(store(m)[1, 1], m[1, 1], forward_store(matrix(0, 2, 2)), "\n")
}
