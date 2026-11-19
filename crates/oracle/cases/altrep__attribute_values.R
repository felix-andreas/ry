# Attributes whose values are ALTREP objects, as `dim<-`, `class<-`, `names<-` and data frames
# store them, read through attr(), class(), inherits(), dim() and the replacement functions.
x <- 1:6
dim(x) <- 2:3
print(attr(x, "dim")); print(dim(x)); print(class(x)); print(inherits(x, "matrix")); print(nrow(x))

k <- 1
class(k) <- as.character(c(10, 20))
print(class(k)); print(inherits(k, "20")); print(inherits(k, c("x", "10"), which = TRUE))
print(oldClass(k)); print(attr(k, "class"))

n <- c(1.5, 2.5)
names(n) <- c(1, 2)
print(attr(n, "names")); print(names(n)); print(attributes(n))

w <- 1:3
attr(w, as.character("tag")) <- "t"
print(attr(w, "tag")); print(attr(w, as.character("tag"), exact = TRUE))

a <- array(1:2, 2)
dimnames(a) <- list(as.character(c(3, 4)))
print(names(a)); print(dimnames(a)); print(a)

s <- setClass("altrepNames", representation(v = "numeric"))
o <- s(v = 1)
r <- tryCatch({ names(o) <- c("a"); "set" }, error = conditionMessage)
print(r)

f <- data.frame(v = 1:3)
print(attr(f, "row.names")); print(rownames(f)); print(dim(f))
