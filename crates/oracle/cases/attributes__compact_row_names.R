# The row names of a data frame with automatic row names read back as 1:n without expanding.
df <- data.frame(a = 1:5, b = letters[1:5])
str(attr(df, "row.names"))
str(attributes(df)$row.names)
r <- attr(df, "row.names")
r[2] <- 10L
str(r)
str(attr(df, "row.names"))
str(attr(data.frame(a = integer()), "row.names"))
str(attr(data.frame(a = 1:2, row.names = c("x", "y")), "row.names"))
head(df, 2)
tail(df, 2)
identical(attr(df, "row.names"), 1:5)
