f <- factor(c("lo", "hi", "mid", "hi"), levels = c("lo", "mid", "hi"))
f; levels(f); nlevels(f); as.integer(f); table(f)
droplevels(f[f != "mid"])
factor(c("b", "a", "c"))
relevel(factor(c("x", "y", "z")), ref = "z")
f2 <- f; levels(f2) <- c("L", "M", "H"); f2
interaction(c("a", "b"), c("x", "y"))
cut(c(1, 5, 9), breaks = c(0, 3, 6, 10))
summary(f)
ordered(c("small", "large"), levels = c("small", "large")) < "large"
tapply(c(10, 20, 30, 40), c("a", "b", "a", "b"), sum)
split(1:6, c(1, 2))
unsplit(split(1:6, c(1, 2)), c(1, 2))
