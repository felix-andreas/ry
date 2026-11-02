l <- list(a = 1, b = "two", c = c(3, 4))
str(l)
l$c[2]
l[["b"]]
l["a"]
names(l)
l$d <- NULL
length(l)
l[["a"]] <- NULL
names(l)
unlist(list(a = 1, b = list(c = 2, d = 3)))
lapply(1:3, function(i) i^2)
sapply(c(x = 1, y = 2), function(v) v * 10)
vapply(1:3, function(i) letters[i], character(1))
Map(function(a, b) a + b, 1:3, 4:6)
Reduce(`+`, 1:5, accumulate = TRUE)
Filter(function(v) v %% 2 == 0, 1:10)
do.call(rbind, list(1:2, 3:4))
rapply(list(1, 2, list(3)), function(v) v * 2, how = "unlist")
setdiff(c("a", "b", "c"), "b")

# unlist numbers unnamed leaves under a tag only when more than one of them shares it.
p <- function(x) print(names(unlist(x)))
p(list(a = list(1, b = 2)))
p(list(a = list(1, 2, b = 3)))
p(list(a = list(1, b = 2, 3)))
p(list(a = list(b = 1, 2)))
p(list(a = list(1, list(2, 3))))
p(list(a = list(c(1, 2), b = 3)))
p(list(a = list(list(1), b = 2)))
p(list(a = list(1, c(b = 2, 3))))
print(names(c(list(a = list(1, b = 2)), recursive = TRUE)))
