df <- data.frame(x = 1:3, y = c("a", "b", "c"), stringsAsFactors = FALSE)
df; nrow(df); ncol(df); names(df); df$y; df[2, ]; df[df$x > 1, "y"]
df$z <- df$x * 2; df
df[["x"]]; df[, c("x", "z")]
rbind(df, data.frame(x = 9L, y = "q", z = 0))
merge(data.frame(k = 1:3, a = c("p", "q", "r")), data.frame(k = c(2, 3, 4), b = c(TRUE, FALSE, TRUE)))
df[order(-df$x), ]
as.list(df[1, ])
lapply(df, class)
subset(df, x >= 2, select = c(y, z))
transform(df, w = x + z)
colnames(df)[2] <- "label"; colnames(df)
