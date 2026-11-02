# oracle: requires lattice
# Lattice's trellis displays, each printed onto the device as interactive use does.
library(lattice)

figure("lattice_xyplot_groups", print(xyplot(Sepal.Length ~ Petal.Length, data = iris,
                                             groups = Species, auto.key = list(columns = 3))))
figure("lattice_xyplot_panels", print(xyplot(mpg ~ disp | factor(cyl), data = mtcars,
                                             type = c("p", "r"), layout = c(3, 1))))
figure("lattice_bwplot", print(bwplot(voice.part ~ height, data = singer)))
figure("lattice_histogram", print(histogram(~ height | voice.part, data = singer, nint = 15)))
figure("lattice_densityplot", print(densityplot(~ eruptions, data = faithful, plot.points = "rug")))
figure("lattice_barchart", print(barchart(yield ~ variety | site, data = barley, groups = year,
                                          stack = TRUE, scales = list(x = list(rot = 45)))))
figure("lattice_dotplot", print(dotplot(variety ~ yield | site, data = barley, groups = year,
                                        layout = c(2, 3))))
figure("lattice_levelplot", print(levelplot(volcano, col.regions = terrain.colors(50))))
figure("lattice_contourplot", print(contourplot(volcano, cuts = 10, labels = TRUE)))
figure("lattice_wireframe", print(wireframe(volcano, shade = TRUE, aspect = c(61 / 87, 0.4))))
figure("lattice_cloud", print(cloud(Sepal.Length ~ Petal.Length * Petal.Width, data = iris,
                                    groups = Species)))
figure("lattice_splom", print(splom(~ iris[1:4], groups = Species, data = iris, pch = 20)))
figure("lattice_parallel", print(parallelplot(~ iris[1:4], data = iris, groups = Species)))
figure("lattice_stripplot", print(stripplot(yield ~ site, data = barley, jitter.data = TRUE)))
figure("lattice_qqmath", print(qqmath(~ height | voice.part, data = singer, prepanel = prepanel.qqmathline,
                                      panel = function(x, ...) {
                                        panel.qqmathline(x, ...)
                                        panel.qqmath(x, ...)
                                      })))
figure("lattice_strip_theme", {
  trellis.par.set(strip.background = list(col = "lightgrey"))
  print(xyplot(lat ~ long | cut(depth, 3), data = quakes, pch = ".", cex = 3,
               strip = strip.custom(strip.names = TRUE)))
})
