# A logical subscript as long as the vector selects in runs of 64, so lengths around each
# boundary, NA flags and every element kind are covered.
for (n in c(0L, 1L, 63L, 64L, 65L, 128L, 129L, 200L)) {
  flags <- rep_len(c(TRUE, FALSE, TRUE, NA, FALSE), n)
  cat("n =", n, "\n")
  print(seq_len(n)[flags])
  print((seq_len(n) / 2)[flags])
  print(as.character(seq_len(n))[flags])
  print((seq_len(n) %% 3L == 0L)[flags])
  print(as.complex(seq_len(n))[flags])
  print(as.raw(seq_len(n) %% 256L)[flags])
  picked <- as.list(seq_len(n))[flags]
  print(vapply(picked, function(e) if (is.null(e)) -1L else e, 0L))
}
x <- c(1.5, NA, NaN, -0, Inf)
print(x[c(NA, TRUE, TRUE, TRUE, FALSE)])
print(1:5 > 2)
print((1:5)[rep(NA, 5)])
print((1:5)[rep(FALSE, 5)])
print(letters[rep(TRUE, 26)])
print(list(1, "a", NULL, 2)[c(TRUE, NA, TRUE, TRUE)])
# Shorter subscripts recycle, longer ones give NA past the end, and names follow the elements.
print((1:10)[c(TRUE, FALSE)])
print((1:10)[TRUE])
print((1:10)[NA])
print(c(a = 1, b = 2, c = 3)[c(TRUE, NA, TRUE)])
print((1:3)[c(TRUE, FALSE, TRUE, TRUE, NA)])
print(letters[1:3][c(FALSE, TRUE, FALSE, TRUE)])
# Data frame rows: an NA row selects an all-NA row named "NA".
df <- data.frame(id = 1:6, value = c(1.5, 2.5, NA, 4.5, 5.5, 6.5), name = letters[1:6],
                 flag = c(TRUE, FALSE, NA, TRUE, TRUE, FALSE))
print(df[df$value > 2, ])
print(df[c(TRUE, FALSE), ])
print(df[df$flag, ])
big <- data.frame(g = c("north", "south", "east", "west")[1:100000 %% 4L + 1L], u = 1:100000 %% 17L,
                  p = 1 + 1:100000 %% 100L)
sub <- big[big$u > 3L, ]
print(dim(sub)); print(head(sub)); print(tail(sub)); print(sum(sub$p)); print(table(sub$g))
