# Top-level loops run in the tree-walking interpreter, which steps through a compact
# sequence such as `1:n` instead of expanding it.
for (i in 1:3) cat(i, class(i), "")
cat("\n")
for (i in 3:1) cat(i, "")
cat("\n")
for (x in 1.5:4) cat(x, class(x), "")
cat("\n")
for (i in seq_len(0)) cat("never")
print(i)
s <- 0
for (i in 1:3000000) s <- s + i
print(s)
print(i)
n <- 0L
for (i in 1:5) {
  i <- i * 10L
  n <- n + i
}
print(c(n, i))
v <- 1:4
for (k in v) {
  v[k] <- 0L
  cat(k, "")
}
cat("\n")
print(v)
for (i in 1:10) if (i == 4) break
print(i)
