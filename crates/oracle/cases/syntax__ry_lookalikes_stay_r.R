# R code that looks like ry's records, tuples, tags and `match` keeps its R meaning:
# none of these is ry syntax, because each one already means something in R.
a <- 2
b <- 4
{a:b}
(a = 1)
a
match(c(3, 1), c(1, 2, 3))
x <- 2
match <- function(x) x * 10
match(x)
{
  x + 1
}
rm(match)
match(x, c(5, 2))
f <- function(v) v
y <- 3
z <- 5
f(y
  :z)
