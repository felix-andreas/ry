# Subsetting and subassignment of arrays over empty, scalar, NA and zero-length subscripts, which
# walk the selected elements in column-major order without first listing their offsets.
x <- array(1:24, c(2, 3, 4))
x[, , 2]
x[, , 2, drop = FALSE]
x[1, , 3]
x[, 2, ]
x[2, 3, ]
x[c(2, 1), , c(4, 1)]
x[NA, , 1]
x[c(1, NA), 2, ]
x[, , c(1, NA)]
x[, 0, ]
dim(x[, , integer(0)])
x[0, , 1, drop = FALSE]
named <- array(as.numeric(1:24), c(2, 3, 4),
               dimnames = list(c("a", "b"), c("p", "q", "r"), paste0("k", 1:4)))
named[, , "k3"]
named["b", , ]
named[, "q", c("k1", "k4"), drop = FALSE]
strings <- array(letters[1:24], c(2, 3, 4))
strings[, , 4]
strings[2, , c(NA, 1)]
listed <- array(as.list(1:8), c(2, 2, 2))
str(listed[, , 2])
complexes <- array(complex(real = 1:8, imaginary = 8:1), c(2, 2, 2))
complexes[, 2, ]

y <- array(0, c(2, 3, 4))
y[, , 2] <- 1:6
y[, , 3] <- c(10, 20)
y[1, , 4] <- -1
y[, 2, ] <- 7L
y
y[, , c(1, NA)] <- 5
y[, , 1]
z <- array(1:24, c(2, 3, 4))
z[c(1, NA), 1, 1] <- 99L
z[, , 1]
z[, 0, ] <- 1L
identical(z, {w <- array(1:24, c(2, 3, 4)); w[1, 1, 1] <- 99L; w})
tryCatch({z[, , c(1, NA)] <- 1:2; "assigned"}, error = conditionMessage)
tryCatch({z[, , 1] <- 1:4; "assigned"}, error = conditionMessage)
s <- array("", c(2, 2, 2))
s[, , 2] <- c("u", "v")
s
l <- array(list(), c(2, 2, 2))
l[, 1, ] <- list("a", 2)
str(l[, , 2])
big <- array(0, c(100, 100, 5))
big[, , 3] <- matrix(1, 100, 100)
c(sum(big[, , 3]), sum(big[, , 2]), sum(big[50, , ]), dim(big[, , 2:3]))
