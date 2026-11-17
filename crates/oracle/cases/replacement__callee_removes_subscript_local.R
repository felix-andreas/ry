# A local assigned earlier in the block and used as a subscript is read before the target only
# while that cannot fail: a callee that removed it through `parent.frame()` leaves the
# assignment to the replacement call, which hands a method the subscript as a promise.
`[<-.k` <- function(x, ..., value) { cat("method [<-", ...length(), "\n"); x }
`[[<-.k` <- function(x, i, value) { cat("method [[<-\n"); x }
drop_k <- function() rm("k", envir = parent.frame())
show <- function(expr) print(tryCatch(expr, error = function(e) conditionMessage(e)))

on_object <- function() {
  z <- structure(1:3, class = "k")
  k <- 1
  drop_k()
  z[k] <- 9L
  z[[k]] <- 9L
  m <- structure(matrix(1:4, 2), class = "k")
  k <- 2
  drop_k()
  m[k, 1] <- 0L
  "done"
}
on_plain <- function() {
  z <- 1:3
  k <- 1
  drop_k()
  z[k] <- 9L
  z
}
# A global `k` is what the subscript reads once the local is gone.
on_global <- function() {
  z <- 1:3
  k <- 1
  drop_k()
  z[k] <- 9L
  z
}
in_loop <- function(n) {
  z <- numeric(n)
  for (i in seq_len(n)) {
    k <- i
    if (i == n) drop_k()
    z[k] <- i * 2
  }
  z
}
for (round in 1:4) {
  show(on_object())
  show(on_plain())
}
k <- 3
for (round in 1:4) show(on_global())
rm(k)
for (round in 1:4) show(in_loop(5))
