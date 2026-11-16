# drop() keeps the dimnames of the kept dimensions together with their names.
x <- array(1:4, c(2, 2, 1),
  dimnames = list(iteration = c("1", "2"), chain = c("a", "b"), variable = "mu"))
str(drop(x))
y <- array(1:4, c(2, 1, 2), dimnames = list(NULL, "z", NULL))
str(drop(y))
z <- array(1:4, c(1, 2, 2), dimnames = list(only = "z", NULL, c("p", "q")))
str(drop(z))
str(drop(array(1:2, c(2, 1), dimnames = list(a = c("p", "q"), b = "z"))))
