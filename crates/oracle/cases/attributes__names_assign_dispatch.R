# `names<-` converts a classed value with the dispatched `as.character()`, so dates and times
# become their printed form; `attr<-`, `attributes<-` and `structure()` convert without
# dispatch and keep the value's class on the names.
x <- list(1, 2)
d <- as.Date(c("2023-05-17", "2023-05-18"))
p <- as.POSIXct(c("2023-05-17 10:00:00", "2023-05-18 00:00:00"), tz = "UTC")
names(x) <- d; names(x)
names(x) <- p; names(x)
names(setNames(1:2, d))
y <- 1:2; names(y) <- factor(c("b", "a")); names(y)
y <- 1:2; names(y) <- c(a = "p", b = "q"); attributes(names(y))
y <- 1:2; names(y) <- c(1.5, 2); names(y)
as.character.myclass <- function(x, ...) paste0("<", unclass(x), ">")
y <- 1:3; names(y) <- structure(1:2, class = "myclass"); names(y)
a <- array(1:2); names(a) <- d; dimnames(a)
y <- 1:2; attr(y, "names") <- d; unclass(names(y))
y <- structure(1:2, names = d); unclass(names(y))
m <- matrix(1:4, 2); rownames(m) <- d; rownames(m)
