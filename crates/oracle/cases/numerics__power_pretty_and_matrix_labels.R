x <- c(-Inf, -2, -1, -0.5, 0, 0.5, 1, 2, Inf, NA, NaN)
y <- c(-Inf, -2, -0.5, 0, 0.5, 1, 3, Inf, NA, NaN)
print(outer(x, y, `^`))
NA_integer_^0L; 2L^NA; (-8)^(1/3); NA^NaN; NaN^NA
pretty(1)
pretty(c(0,1))
pretty(1e-300)
x <- c(0, 1e-300); x; format(x); print(c(a=1e100, b=1)); matrix(c(1e-300, 1, 2, 3e200), 2); summary(c(1e-300,1))
a <- rep(1:11, 1:11)[1:20]; b <- rep(1:2, 10)
table(a, b)
table(a[1:5], b[1:5])
m <- matrix(1:4, 2, dimnames=list(c("x","y"), c("p","q"))); names(dimnames(m)) <- c("", ""); m
names(dimnames(m)) <- c("rows", ""); m
names(dimnames(m)) <- c("", "cols"); m
matrix(1:22, 11)
matrix(c(1.5,2,NA,4), 2, dimnames=list(c("a","bbb"), NULL))
matrix(nrow=11, ncol=0); x <- matrix(1:22, 11); x; x[, 1, drop=FALSE]; table(rep(1:11, 1:11), rep(1:2, 33))
x <- c(-100,-3:2, -99.9, -7.7, seq(-3,3, length=61), 5.1, 77)
stopifnot(all.equal( digamma(x), psigamma(x,0), tolerance = 2*.Machine$double.eps), all.equal(trigamma(x), psigamma(x,1), tolerance = 2*.Machine$double.eps))
digamma(c(1, 0.5, 0, -1, 10)); trigamma(c(1, 0.5, 0, -1, 10))
