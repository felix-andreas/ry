# A matrix or array assignment skips NA subscripts when the value is a single element, and
# refuses them for a longer value.
m <- matrix(1:24, 4, 6)
m[c(NA, NA, NA, NA), 1] <- 5L
print(m[, 1])
m[NA, 1] <- 5L
print(m[, 1])
m[c(NA, TRUE, NA, FALSE), 2] <- 3L
print(m[, 2])
r <- try(m[c(1, NA, 3), 3] <- c(10L, 20L, 30L), silent = TRUE); cat(r)
print(m[, 3])
m[matrix(NA, 4, 1), ] <- 1
print(m[1, ])
r <- try(m[c(1, NA), 4] <- 1:3, silent = TRUE); cat(r)
r <- try(m[c(NA, NA), 4] <- integer(0), silent = TRUE); cat(r)
a <- array(1:8, c(2, 2, 2)); a[c(NA, 2), 1, ] <- 0L; print(a)
r <- try(a[c(NA, 2), 1, ] <- c(0L, 9L), silent = TRUE); cat(r)
l <- matrix(list(1, 2, 3, 4), 2); l[c(NA, 2), 1] <- list("x"); str(l)
