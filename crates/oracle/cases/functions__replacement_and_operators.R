"second<-" <- function(x, value) { x[2] <- value; x }
v <- 1:3
second(v) <- 99L
v
l <- list(a = list(b = 1))
l$a$b <- 2
l[["a"]][["c"]] <- 3
str(l)
names(l)[1] <- "z"
names(l)
x <- 1:5
x[x > 3] <- 0L
x
ff <- factor(c("a", "b"))
levels(ff)[2] <- "B"
ff
`attr<-`(1, "k", "v")
m <- matrix(0, 2, 2)
m[1, ] <- c(5, 6)
diag(m) <- 1
m
is.primitive(sum)
`+`(1, 2)
sapply(list(`-`, `*`), function(op) op(6, 3))
identical(c(a = 1), c(a = 1))
identical(1L, 1)
