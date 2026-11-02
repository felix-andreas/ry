# lattice panels drawn through grid on a ragg device, compared with GNU R by pixel checksum.
library(lattice); library(ragg)
shot <- function(p) { agg_capture(width = 300, height = 240); print(p); v <- dev.capture(native = TRUE); invisible(dev.off()); cat(length(unique(as.vector(v))), sum(as.numeric(v) %% 65521), "\n") }
shot(xyplot(mpg ~ wt | factor(cyl), data = mtcars))
shot(levelplot(volcano))
shot(bwplot(Species ~ Sepal.Length, iris))
shot(histogram(~ Sepal.Width, iris))
