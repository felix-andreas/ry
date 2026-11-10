# Growing a vector past its end copies the existing elements into a new vector of the same type:
# every atomic type, lists and strings with names, a compact sequence, and repeated growth.
x <- 1:3; x[6] <- 9L; print(x)
r <- c(a = 1.5, b = 2.5); r[["d"]] <- 4; print(r)
z <- complex(real = 1:2, imaginary = 3:4); z[4] <- 1i; print(z)
b <- as.raw(1:2); b[4] <- as.raw(255); print(b)
l <- TRUE; l[3] <- FALSE; print(l)
s <- c(p = "x", q = "y"); s[4] <- "w"; print(s)
items <- list(a = list(1), b = "two"); items[[4]] <- 4; str(items)
acc <- list(); for (i in 1:2000) acc[[i]] <- i; print(length(acc)); print(sum(unlist(acc)))
v <- numeric(); for (i in 1:2000) v[i] <- i / 2; print(sum(v))
shared <- c(1, 2); copy <- shared; copy[4] <- 4; print(shared); print(copy)
