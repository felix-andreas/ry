# Top-level `for` loops over compact sequences on both sides of the length at which a loop
# compiles: a short loop steps through the sequence in the tree walker, a long one runs compiled.
# Each loop reads every element, and the sequence keeps its type and values afterwards.
s <- 0L
for (i in 1:10) s <- s + i
print(c(s, i)); print(typeof(i))

d <- 0
for (x in as.double(1:10)) d <- d + x
print(c(d, x, typeof(x)))

h <- 0
for (x in 1.5:9.5) h <- h + x
print(c(h, x))

down <- integer()
for (i in 5:-3) down <- c(down, i)
print(down)

flags <- 0L
for (b in c(TRUE, NA, FALSE, TRUE)) flags <- flags + if (is.na(b)) 10L else b
print(flags)

one <- NULL
for (i in 7:7) one <- i
print(one)

# 15, 16 and 17 elements straddle the compile threshold.
for (n in c(15L, 16L, 17L)) {
  t <- 0L
  last <- NA
  for (i in seq_len(n)) { t <- t + i; last <- i }
  print(c(n, t, last))
}
s <- 0L
for (i in 1:16) s <- s + i
print(c(s, i))
s <- 0L
for (i in 1:17) s <- s + i
print(c(s, i))
r <- 0
for (x in 0.5:16.5) r <- r + x
print(c(r, x))
r <- 0
for (x in as.double(3:20)) r <- r + x
print(c(r, x, typeof(x)))
lg <- 0L
for (b in rep(c(TRUE, FALSE), 9)) lg <- lg + b
print(lg)

# Writing to the variable that holds the sequence leaves the remaining iterations unchanged,
# short and long.
v <- 1:10
seen <- integer()
for (i in v) { v[10] <- 0L; seen <- c(seen, i) }
print(seen); print(v)
w <- 1:20
seen <- integer()
for (i in w) { w[20] <- 0L; seen <- c(seen, i) }
print(seen); print(w)
