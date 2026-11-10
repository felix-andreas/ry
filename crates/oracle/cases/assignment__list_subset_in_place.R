# `x[i] <- list(...)` on a list a function owns, in an environment frame and in a slot frame,
# with sharing, recycling, NULL elements, names, out-of-range and NA indices, and a value that
# holds the list itself.
run <- function() {
  x <- list(1, "a", NULL, 4)
  x[2] <- list(5)
  x[c(1, 4)] <- list("p", "q")
  x[3] <- list(NULL)
  y <- x
  x[1] <- list(99)
  print(y[[1]]); print(x[[1]])
  x[1] <- list(x)
  str(x[[1]])
  z <- list(1, 2)
  z[c(TRUE, FALSE)] <- list(list(7))
  str(z)
  w <- list(a = 1, b = 2)
  w[2] <- list(3:4)
  str(w)
  w[5] <- list(1)
  str(w)
  v <- list(1, 2, 3, 4)
  v[1:4] <- list(0, 9)
  str(v)
  v <- withCallingHandlers(
    { v[1:3] <- list(0, 9); v },
    warning = function(w) { print(conditionMessage(w)); invokeRestart("muffleWarning") }
  )
  str(v)
  v[2] <- list(c(a = 1))
  inner <- v[[2]]; v[[2]][1] <- 10; print(inner); print(v[[2]])
  e <- list(1, 2); e[2.9] <- list("r"); str(e)
  s <- list(1, 2); s[NA_integer_] <- list(3); str(s)
  n <- list(1, 2); print(tryCatch({ n[2] <- list(); n }, error = conditionMessage))
  m <- list(1, 2, 3); m[2] <- NULL; str(m)
  k <- list(1, 2); k[1] <- 5; str(k)
  l <- list(1, 2); l[1] <- list(e = 3); str(l)
  cls <- structure(list(1, 2), class = "box"); cls[1] <- list(8); print(unclass(cls))
  ex <- expression(a, b); ex[2] <- list(quote(c)); print(ex)
  invisible()
}
for (round in 1:3) run()
f <- function(x) { x[2] <- list("in"); x }
shared <- list(1, 2)
print(f(shared)[[2]]); print(shared[[2]])
g <- function() { acc <- vector("list", 3); for (i in 1:3) acc[i] <- list(i * 2); acc }
for (round in 1:3) str(g())
