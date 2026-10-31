chull(c(0, 1, 1, 0, 0.5), c(0, 0, 1, 1, 0.5))
chull(c(0, 1, 2, 2, 0, 0), c(0, 0, 0, 2, 2, 0))
chull(sin(1:20 * 1.3), cos((1:20)^1.5))
contourLines(1:3, 1:3, matrix(c(1, 2, 3, 2, 3, 4, 3, 4, 5), 3), levels = c(2.5, 3.5))
show <- function(lines) for (l in lines) cat(l$level, ":", round(l$x, 3), "|", round(l$y, 3), "\n")
show(contourLines(z = outer(1:7, 1:6, function(a, b) -((a - 4)^2 + (b - 3.5)^2)), levels = c(-6, -2, -9)))
show(contourLines(z = matrix(c(0, 1, 0, 1, 0, 1, 0, 1, 0), 3), levels = 0.5))
surface <- outer(seq(0, 6, length.out = 15), seq(0, 5, length.out = 12), function(a, b) sin(a) * cos(b) + 0.3 * sin(a * b))
show(contourLines(z = surface, nlevels = 7))
surface[3, 4] <- NA
show(contourLines(z = surface, levels = c(-0.2, 0.4)))
show(contourLines(c(0, 1), c(0, 1), matrix(c(0, 1, 2, 3), 2), levels = 1))
for (u in list(c(0, 10), c(0.3, 7.9), c(-5.2, 103), c(1e-5, 3e-5), c(1000, 1001), c(7, 7.5))) {
  print(unlist(.axisPars(u)))
  print(axisTicks(u, log = FALSE))
  print(axisTicks(u, log = FALSE, nint = 3))
}
for (u in list(c(-1, 3), c(0.1, 2), c(0, 0.5), c(-10, 10), c(1, 2.5), c(-20, 30), c(2, 8))) {
  print(unlist(.axisPars(u, log = TRUE)))
  print(axisTicks(u, log = TRUE))
  print(axisTicks(u, log = TRUE, nint = 10))
}
for (n in c(3, 8)) for (d in c(10, 13, 15, 21, 30)) cat(n, d, ":", log10(axisTicks(c(0, d), log = TRUE, nint = n)), "\n")
x <- c(1, 2, 2, 3, 3, 3, 4, 4, 5, 10, 0.5, 7.25)
h <- hist(x, plot = FALSE)
h$breaks
h$counts
h$density
hist(x, breaks = c(0, 2, 4, 10), plot = FALSE, right = FALSE)$counts
hist(c(1, 2, 3, NA, Inf), breaks = 1:3, plot = FALSE)$counts
dev.cur()
dev.list()
dev.next()
dev.prev()
tryCatch(dev.off(), error = conditionMessage)
identical(getOption("device"), pdf)
n2mfrow(7)
extendrange(c(1, 5), f = 0.1)
boxplot.stats(c(1:10, 50))
nclass.Sturges(1:100)
