# Row and column names made by coercion must be found by name, as literal ones are.
m <- matrix(1:6, nrow = 3)
rownames(m) <- 1:3
colnames(m) <- c(10.5, 20.5)
print(m["2", "20.5"])
print(m[["3", "10.5"]])
print(m[c("1", "3"), ])
print(m[cbind(c("2", "3"), c("10.5", "20.5"))])
print(match("2", rownames(m)))
dimnames(m) <- list(sprintf("%d", 1:3), paste0("", 1:2))
print(m["3", "2"])
m["1", "1"] <- 100L
print(m)
a <- array(1:24, dim = 2:4, dimnames = list(1:2, 1:3, 1:4))
print(a["2", "3", "4"])
print(a[["1", "2", "3"]])
one <- array(1:3, dim = 3, dimnames = list(1:3))
print(one["2"])
print(one[["3"]])

# Data frames: integer row names read as strings.
df <- data.frame(x = 1:3, y = c("a", "b", "c"))
print(df["2", ])
print(df[as.character(3L), "y"])
rownames(df) <- c(7, 8, 9)
print(df["8", "x"])
print(df[["y"]][match("9", rownames(df))])
