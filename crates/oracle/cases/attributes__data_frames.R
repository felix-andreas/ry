df <- data.frame(id = 1:4, g = c("a", "b", "a", "b"), v = c(2.5, NA, 7, 1), stringsAsFactors = FALSE)
df
str(df)
nrow(df); ncol(df)
df[df$g == "a", ]
df$w <- df$v * 2
colnames(df)
aggregate(v ~ g, data = df, FUN = sum)
merge(df, data.frame(g = c("a", "b"), label = c("Alpha", "Beta")))
df[order(-df$id), "id"]
summary(df$v)
sapply(df, class)
with(df, tapply(v, g, mean, na.rm = TRUE))
do.call(rbind, lapply(split(df, df$g), function(d) data.frame(g = d$g[1], n = nrow(d))))
