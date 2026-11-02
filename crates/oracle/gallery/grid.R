# Grid drawn directly: primitives, viewports and layouts, units, graphical parameters, and
# the newer fills (gradients, patterns), clipping paths and masks.
library(grid)

figure("grid_primitives", {
  grid.newpage()
  grid.rect(gp = gpar(col = "grey"))
  grid.lines(c(0.1, 0.9), c(0.1, 0.9), gp = gpar(col = "red", lwd = 2))
  grid.circle(0.5, 0.5, r = 0.2, gp = gpar(fill = "steelblue", alpha = 0.6))
  grid.polygon(c(0.1, 0.3, 0.2), c(0.7, 0.7, 0.9), gp = gpar(fill = "orange"))
  grid.points(seq(0.1, 0.9, length.out = 9), rep(0.2, 9), pch = 1:9, size = unit(4, "mm"))
  grid.segments(0.6, 0.8, 0.9, 0.95, arrow = arrow(type = "closed"), gp = gpar(fill = "black"))
})
figure("grid_viewports", {
  grid.newpage()
  pushViewport(viewport(width = 0.8, height = 0.8, xscale = c(0, 100), yscale = c(-1, 1)))
  grid.rect()
  grid.xaxis()
  grid.yaxis()
  grid.lines(unit(0:100, "native"), unit(sin(0:100 / 10), "native"), gp = gpar(col = "navy"))
  pushViewport(viewport(x = 0.75, y = 0.75, width = 0.3, height = 0.3, angle = 15))
  grid.rect(gp = gpar(fill = "lightyellow"))
  grid.text("rotated viewport")
  popViewport(2)
})
figure("grid_layout", {
  grid.newpage()
  pushViewport(viewport(layout = grid.layout(3, 3, widths = unit(c(2, 1, 1), c("cm", "null", "null")),
                                             heights = unit(c(1, 3, 1), "null"))))
  for (row in 1:3) for (column in 1:3) {
    pushViewport(viewport(layout.pos.row = row, layout.pos.col = column))
    grid.rect(gp = gpar(fill = hcl(30 * (row * 3 + column), 50, 80), col = "white"))
    grid.text(paste(row, column))
    popViewport()
  }
  popViewport()
})
figure("grid_text_justification", {
  grid.newpage()
  grid.segments(0.5, 0, 0.5, 1, gp = gpar(col = "grey"))
  grid.segments(0, 0.5, 1, 0.5, gp = gpar(col = "grey"))
  for (just in list(c("left", "bottom"), c("right", "top"), c("centre", "centre")))
    grid.text(paste(just, collapse = "-"), 0.5, 0.5, just = just, gp = gpar(fontsize = 16))
  grid.text("bold serif", 0.2, 0.85, gp = gpar(fontface = "bold", fontfamily = "serif"))
  grid.text("rotated 90", 0.9, 0.5, rot = 90, gp = gpar(cex = 1.3, col = "darkgreen"))
})
figure("grid_round_rect_curve", {
  grid.newpage()
  grid.roundrect(0.3, 0.5, 0.4, 0.5, r = unit(0.1, "snpc"), gp = gpar(fill = "plum"))
  grid.curve(0.6, 0.2, 0.9, 0.8, curvature = -0.6, arrow = arrow(), square = FALSE)
  grid.bezier(c(0.55, 0.65, 0.8, 0.95), c(0.9, 0.3, 0.9, 0.3), gp = gpar(lwd = 2, col = "blue"))
})
figure("grid_linear_gradient", {
  grid.newpage()
  grid.rect(width = 0.8, height = 0.4, gp = gpar(fill = linearGradient(c("navy", "white", "red"))))
  grid.circle(0.5, 0.8, 0.15, gp = gpar(fill = radialGradient(c("yellow", "orange", "darkred"))))
})
figure("grid_pattern", {
  grid.newpage()
  tile <- pattern(circleGrob(r = unit(2, "mm"), gp = gpar(fill = "darkcyan", col = NA)),
                  width = unit(6, "mm"), height = unit(6, "mm"), extend = "repeat")
  grid.rect(width = 0.7, height = 0.7, gp = gpar(fill = tile))
})
figure("grid_clip_path", {
  grid.newpage()
  pushViewport(viewport(clip = circleGrob(r = 0.35)))
  grid.rect(gp = gpar(fill = "gold"))
  grid.segments(seq(0, 1, 0.05), 0, seq(0, 1, 0.05), 1, gp = gpar(col = "brown", lwd = 2))
  popViewport()
})
figure("grid_mask", {
  grid.newpage()
  mask <- textGrob("MASK", gp = gpar(fontsize = 80, fontface = "bold"))
  pushViewport(viewport(mask = mask))
  grid.rect(gp = gpar(fill = linearGradient(c("purple", "orange"))))
  popViewport()
})
figure("grid_frame_pack", {
  grid.newpage()
  frame <- frameGrob()
  frame <- packGrob(frame, rectGrob(gp = gpar(fill = "grey90")))
  frame <- packGrob(frame, textGrob("legend", rot = 90), side = "right", width = unit(1.5, "cm"))
  frame <- packGrob(frame, textGrob("Title", gp = gpar(fontsize = 18)), side = "top",
                    height = unit(1.5, "cm"))
  grid.draw(frame)
})
figure("grid_gtree_edit", {
  grid.newpage()
  tree <- gTree(children = gList(rectGrob(name = "box", gp = gpar(fill = "skyblue")),
                                 textGrob("edited", name = "label")), name = "tree",
                vp = viewport(width = 0.5, height = 0.3))
  grid.draw(tree)
  grid.edit("tree::box", gp = gpar(fill = "salmon", lwd = 3))
  grid.edit("tree::label", label = "after edit")
})
figure("grid_units_arithmetic", {
  grid.newpage()
  grid.rect(x = unit(1, "npc") - unit(2, "cm"), width = unit(3, "cm") + unit(0.1, "npc"),
            height = max(unit(2, "lines"), unit(1, "inches")), just = "right")
  grid.text("strwidth", x = unit(0.2, "npc") + stringWidth("strwidth") * 0.5, y = 0.2)
})
figure("grid_path_rule", {
  grid.newpage()
  angle <- seq(0, 2 * pi, length.out = 6)[-6]
  grid.path(c(0.5 + 0.4 * cos(angle), 0.5 + 0.2 * cos(angle)),
            c(0.5 + 0.4 * sin(angle), 0.5 + 0.2 * sin(angle)), id = rep(1:2, each = 5),
            rule = "evenodd", gp = gpar(fill = "darkcyan"))
})
figure("grid_raster", {
  grid.newpage()
  grid.raster(matrix(hcl.colors(20), 4, 5), interpolate = FALSE, width = 0.6)
  grid.raster(volcano / max(volcano), x = 0.85, y = 0.2, width = 0.25)
})
