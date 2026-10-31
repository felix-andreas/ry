for (i in seq_len(3)) cat("i =", i, "\n")
i
n <- 0
while (TRUE) { n <- n + 1; if (n >= 3) break }
n
repeat { n <- n - 1; if (n == 0) break }
n
switch("b", a = "A", b = "B", "other")
switch("z", a = "A", "other")
switch("a", a = , b = "A or B")
switch(2, "x", "y", "z")
is.null(switch("q", a = 1))
f <- function(x) { if (x > 0) return("pos"); "non-pos" }
f(1); f(-1)
for (nm in names(c(a = 1, b = 2))) print(nm)
x <- NULL
for (v in list(1, "a", TRUE)) x <- c(x, class(v))
x
(function(n) { if (n <= 1) 1 else n * sys.function()(n - 1) })(5)
vapply(1:3, function(i) i * 2, numeric(1))
seq(2, 20, length.out = 4)
