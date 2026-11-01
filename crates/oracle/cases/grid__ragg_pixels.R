# Grid and base graphics drawn on a ragg device, compared pixel for pixel through a checksum
# of the captured raster. ragg rasterizes, so equal pixels mean the engine handed it the same
# shapes, coordinates, clipping and graphical parameters as GNU R's engine does.
library(grid)
library(ragg)
summarise <- function(raster) {
  values <- as.vector(raster)
  colours <- sort(unique(values))
  cat(dim(raster), length(colours), sum(as.numeric(values) %% 65521), "\n")
  print(sort(tabulate(match(values, colours)), decreasing = TRUE)[1:6])
}
shot <- function(draw) {
  agg_capture(width = 240, height = 180)
  draw()
  raster <- dev.capture(native = TRUE)
  invisible(dev.off())
  summarise(raster)
}
shot(function() {
  pushViewport(viewport(width = 0.8, height = 0.8, xscale = c(0, 10), yscale = c(0, 5)))
  grid.rect(gp = gpar(fill = "grey90"))
  grid.lines(unit(c(0, 10), "native"), unit(c(0, 5), "native"), gp = gpar(col = "red", lwd = 2))
  grid.circle(unit(5, "native"), unit(2.5, "native"), r = unit(1, "cm"),
              gp = gpar(fill = "steelblue"))
  grid.polygon(c(0.1, 0.3, 0.2), c(0.1, 0.1, 0.4), gp = gpar(fill = "orange", col = NA))
  pushViewport(viewport(x = 0.9, y = 0.9, width = 0.3, height = 0.3, clip = "on"))
  grid.rect(width = 2, height = 2, gp = gpar(fill = "darkgreen"))
  popViewport(2)
  grid.segments(0, 0, 1, 1, gp = gpar(lty = "dashed"))
})
shot(function() {
  grid.text("rotated", x = 0.3, rot = 45, gp = gpar(fontsize = 14))
  grid.text("right", x = 0.9, y = 0.2, just = "right", gp = gpar(fontface = "bold"))
  grid.points(seq(0.1, 0.9, length.out = 9), rep(0.8, 9), pch = 1:9)
  grid.xaxis(vp = viewport(y = 0.5, height = 0.4, width = 0.8))
})
shot(function() {
  layout <- grid.layout(2, 2, widths = unit(c(1, 2), "null"))
  pushViewport(viewport(layout = layout))
  for (row in 1:2) for (column in 1:2) {
    pushViewport(viewport(layout.pos.row = row, layout.pos.col = column))
    grid.rect(gp = gpar(fill = hcl(90 * (row + column)), col = "white", lwd = 3))
    popViewport()
  }
  popViewport()
})
shot(function() plot(1:10, main = "Title", xlab = "x", ylab = "y"))
shot(function() {
  par(mfrow = c(1, 2), mar = c(2, 2, 1, 1))
  barplot(c(3, 5, 2), col = c("red", "green", "blue"))
  hist(c(1, 2, 2, 3, 3, 3, 4, 4, 5), main = "")
})
shot(function() {
  plot.new()
  plot.window(c(0, 1), c(0, 1))
  polygon(c(0.1, 0.9, 0.5), c(0.1, 0.1, 0.9), col = "#FF000080", border = "black", lty = 2)
  rect(0.2, 0.2, 0.4, 0.6, density = 10, angle = 30)
  symbols(0.7, 0.5, circles = 0.1, inches = FALSE, add = TRUE, bg = "yellow")
  arrows(0, 1, 1, 0, length = 0.1, lwd = 2)
})
