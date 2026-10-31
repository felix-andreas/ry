"second<-" <- function(x, value) { x[2] <- value; x }
v <- 1:5
second(v) <- 100L
v
x <- list(a = list(b = list(c = 1)))
x$a$b$c <- 2
x$a$b$c
names(x)[1] <- "renamed"
names(x)
m <- matrix(0, 2, 2); m[1, ] <- c(5, 6); m
attr(v, "note") <- "hello"; attributes(v)
y <- v; y[1] <- -1L; v[1]; y[1]
f <- function(z) { z[1] <- 999; z }
orig <- c(1, 2, 3); f(orig); orig
l <- list(1, 2); l[[3]] <- "three"; length(l)
l[["new"]] <- TRUE; names(l)
l[2] <- NULL; length(l)
e <- new.env(); e$val <- 1; g <- function(env) env$val <- 2; g(e); e$val
fac <- factor(c("a", "b", "a")); levels(fac)[2] <- "B"; fac
