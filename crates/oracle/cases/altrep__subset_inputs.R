# Subsetting where the vector, its names, its dims or its dimnames are ALTREP objects: compact
# sequences, deferred string conversions and the wrappers sort() and attribute changes return.
long <- sort(c(100:1, 0.5))
wrapped <- sort(c(70:1 + 0.5))
deferred <- as.character(c(10, 20, 30))
compact <- 1:5

# Reads from ALTREP vectors.
print(long[[3]]); print(long[c(1, 101)]); print(wrapped[-(1:68)])
print(deferred[[2]]); print(deferred[2:3]); print(compact[[4]]); print(compact[-1])
print(rev(compact)[2]); print(seq(2, 10, by = 2)[[3]])

# ALTREP subscripts.
x <- c(a = 1, b = 2, c = 3, d = 4, e = 5)
print(x[2:3]); print(x[[as.character("b")]]); print(x[as.character(c("e", "a"))])
print(x[sort(c(4L, 2L))]); print(x[-(1:2)])

# Names that are ALTREP: integer and double names, a wrapper of strings.
y <- as.list(1:3); names(y) <- 1:3
print(y[["2"]]); print(y$`3`); print(y[c("1", "3")])
z <- setNames(as.list(1:70), 70:1)
print(z[["35"]]); print(names(z[c("70", "1")]))

# Dims and dimnames that are ALTREP.
m <- 1:6; dim(m) <- 2:3
print(m[2, 3]); print(m[, 2]); print(m[[1, 2]]); print(dim(m[, 2:3]))
d <- matrix(1:6, 2); dimnames(d) <- list(1:2, as.character(c(7, 8, 9)))
print(d["2", "8"]); print(d[, "9"]); print(d[["1", "7"]]); print(dimnames(d[, c("8", "9")]))
a <- array(1:3, 3); dimnames(a) <- list(as.character(c(4, 5, 6)))
print(a["5"]); print(a[2:3]); print(names(a))
d[["2", "9"]] <- 60L; print(d)
print(tryCatch(m[[1, ]], error = conditionMessage))
l <- as.list(1:6); dim(l) <- 2:3
print(tryCatch(l[[1, ]], error = conditionMessage)); print(l[[2, 3]])
a3 <- 1:24; dim(a3) <- 2:4
print(a3[2, 3, 4]); print(a3[, 1, 1]); print(dim(a3[, , 1:2]))
dimnames(a3) <- list(as.character(1:2), as.character(c(5, 6, 7)), NULL)
print(a3["2", "6", 1]); a3["1", "7", 4] <- 0L; print(a3[, , 4])
v <- 1:3; dim(v) <- 3L; dimnames(v) <- list(as.character(c(8, 9, 10)))
print(v[2]); print(v["9"]); print(names(v)); print(v[2, drop = TRUE])

# Data frame row names are a compact sequence.
f <- data.frame(v = c(1.5, 2.5, 3.5))
print(f[2, ]); print(f[["v"]][2:3]); print(.row_names_info(f)); print(dim(f[f$v > 2, , drop = FALSE]))
