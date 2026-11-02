library(grid)
directory <- tempfile()
dir.create(directory)
scene <- function() {
  grid.newpage()
  grid.rect(gp = gpar(fill = linearGradient(c("red", "blue"))))
  grid.circle(r = 0.3, gp = gpar(fill = radialGradient(c("white", "black"))))
  tile <- pattern(circleGrob(r = 0.2, gp = gpar(fill = "orange")),
                  width = 0.1, height = 0.1, extend = "repeat")
  pushViewport(viewport(clip = circleGrob(r = 0.4)))
  grid.rect(gp = gpar(fill = tile))
  popViewport()
  pushViewport(viewport(mask = rectGrob(width = 0.5, gp = gpar(fill = "black"))))
  grid.text("masked", gp = gpar(fontsize = 30))
  popViewport()
  grid.path(c(0.1, 0.9, 0.9, 0.1, 0.3, 0.7, 0.7, 0.3),
            c(0.1, 0.1, 0.9, 0.9, 0.3, 0.3, 0.7, 0.7),
            id = rep(1:2, each = 4), rule = "evenodd", gp = gpar(fill = "grey"))
  grid.group(circleGrob(gp = gpar(fill = "green")), "multiply",
             rectGrob(gp = gpar(fill = "yellow")))
  grid.fill(circleGrob(r = 0.1), gp = gpar(fill = "purple"))
  grid.stroke(rectGrob(width = 0.2), gp = gpar(col = "navy", lwd = 3))
}
capabilities <- c("patterns", "clippingPaths", "masks", "compositing",
                  "transformations", "paths", "glyphs")
for (device in c("pdf", "svg", "png")) {
  file <- file.path(directory, paste0("scene.", device))
  do.call(device, list(file))
  scene()
  if (device == "pdf") print(str(dev.capabilities()[capabilities]))
  invisible(dev.off())
  cat(device, file.exists(file) && file.size(file) > 0, "\n")
}
pdf(file.path(directory, "operator.pdf"))
grid.group(rectGrob(gp = gpar(fill = "red")), "clear")
invisible(dev.off())
pdf(file.path(directory, "mixed.pdf"))
grid.fill(gTree(children = gList(textGrob("a"), rectGrob())))
invisible(dev.off())
