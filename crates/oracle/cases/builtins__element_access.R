# [[ and [[<- on vectors and lists: bounds, negative and zero indices, names, NA, exact,
# recursion, NULL, coercion and growth.

l <- list(a = 1, b = "x", c = NULL, d = list(e = 2))
l[[1]]
l[[1L]]
l[[2.9]]
l[["b"]]
l[["d"]][["e"]]
l[[c(4, 1)]]
l[[c("d", "e")]]
l[["z"]]
l[["a", exact = FALSE]]
l[["ab", exact = FALSE]]
l[["a", exact = NA]]
tryCatch(l[[5]], error = conditionMessage)
tryCatch(l[[0]], error = conditionMessage)
tryCatch(l[[-1]], error = conditionMessage)
tryCatch(l[[-5]], error = conditionMessage)
tryCatch(l[[NA]], error = conditionMessage)
l[[NA_character_]]
tryCatch(l[[NA_integer_]], error = conditionMessage)
tryCatch(l[[NA_real_]], error = conditionMessage)
tryCatch(l[[c(1, 2)]], error = conditionMessage)
tryCatch(l[[TRUE]], error = conditionMessage)
tryCatch(l[[list(1)]], error = conditionMessage)
tryCatch(l[[]], error = conditionMessage)
tryCatch(l[[1.5e10]], error = conditionMessage)
tryCatch(list()[[1]], error = conditionMessage)
list(1, 2)[[-1]]
tryCatch(list(1, 2, 3)[[-1]], error = conditionMessage)
NULL[[1]]
NULL[["a"]]
v <- c(a = 1, b = 2, c = 3)
v[[2]]
v[["c"]]
tryCatch(v[["z"]], error = conditionMessage)
tryCatch(v[[4]], error = conditionMessage)
tryCatch(v[[0]], error = conditionMessage)
c(1, 2)[[-1]]
tryCatch(c(1, 2, 3)[[-2]], error = conditionMessage)
tryCatch(v[[NA]], error = conditionMessage)
tryCatch(v[[NA_integer_]], error = conditionMessage)
(1:5)[[3]]
(1:5)[[3.7]]
letters[[26]]
c(TRUE, NA)[[2]]
as.raw(1:3)[[2]]
c(1i, 2i)[[2]]
tryCatch(sum[[1]], error = conditionMessage)
quote(f(x, y))[[2]]
quote(f(x, y))[[1]]
pairlist(a = 1, b = 2)[["b"]]
expression(a, b + 1)[[2]]
e <- new.env()
assign("k", 42, envir = e)
e[["k"]]
e[["nope"]]
tryCatch(e[[1]], error = conditionMessage)
m <- matrix(1:6, 2, dimnames = list(c("r1", "r2"), c("x", "y", "z")))
m[[2, 3]]
m[["r1", "y"]]
m[[5]]
tryCatch(m[[3, 1]], error = conditionMessage)
df <- data.frame(a = 1:2, b = c("u", "v"))
df[["b"]]
df[[2, "a"]]
df[[1]]

x <- list(a = 1)
x[["b"]] <- 2
x[[4]] <- 4
str(x)
x[["a"]] <- NULL
str(x)
x[[10]] <- NULL
length(x)
x[[1]] <- list(9)
str(x)
x[["b"]] <- list()
str(x)
y <- 1:3
y[[2]] <- 10L
y
typeof(y)
y[[2]] <- 2.5
y
typeof(y)
y[[5]] <- 1
y
y[["n"]] <- 7
y
y[[1]] <- "a"
y
tryCatch({ y[[1]] <- 1:2; y }, error = conditionMessage)
tryCatch({ y[[0]] <- 1; y }, error = conditionMessage)
tryCatch({ y[[-1]] <- 1; y }, error = conditionMessage)
tryCatch({ y[[NA]] <- 1; y }, error = conditionMessage)
z <- c(TRUE, FALSE)
z[[2]] <- NA
z
z[[1]] <- 3L
z
typeof(z)
lg <- c(a = TRUE)
lg[["b"]] <- 1i
lg
n <- NULL
n[[1]] <- 5
n
n2 <- NULL
n2[["a"]] <- "x"
n2
n3 <- NULL
n3[[2]] <- list(1)
str(n3)
r <- list(p = list(q = 1))
r[[c("p", "q")]] <- 2
str(r)
r[[c(1, 1)]] <- 3
str(r)
a <- list(1, 2)
b <- a
b[[1]] <- "changed"
a[[1]]
cl <- quote(f(x))
cl[[2]] <- quote(y)
cl
cl[[3]] <- 1
cl
ee <- new.env()
ee[["v"]] <- 1
get("v", envir = ee)
tryCatch({ ee[[1]] <- 2 }, error = conditionMessage)
m2 <- matrix(0, 2, 2)
m2[[1, 2]] <- 5
m2
s <- structure(list(a = 1), class = "rec")
`[[.rec` <- function(x, i) paste("get", i)
`[[<-.rec` <- function(x, i, value) { attr(x, "set") <- i; x }
s[["a"]]
s[["a"]] <- 2
attr(s, "set")
rm(`[[.rec`, `[[<-.rec`)
f <- factor(c("u", "v"))
f[[2]]
tryCatch({ f[[1]] <- "w"; f }, warning = conditionMessage)
