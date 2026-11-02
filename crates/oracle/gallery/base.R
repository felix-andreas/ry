# Base graphics as analysis code draws it: the high-level plot functions of graphics and
# stats, their common arguments, and the low-level annotation calls added on top.
x <- seq(-3, 3, length.out = 60)

figure("scatter_default", plot(1:20, (1:20)^1.5))
figure("scatter_mtcars", {
  plot(mpg ~ wt, data = mtcars, pch = 19, col = factor(cyl), main = "Fuel economy",
       xlab = "Weight (1000 lbs)", ylab = "Miles per gallon")
  legend("topright", legend = levels(factor(mtcars$cyl)), col = 1:3, pch = 19, title = "cyl")
})
figure("plot_types", {
  par(mfrow = c(2, 4), mar = c(2, 2, 2, 1))
  for (type in c("p", "l", "b", "c", "o", "h", "s", "S")) plot(sin(1:12), type = type, main = type)
})
figure("pch_table", {
  plot(0:25 %% 9, 0:25 %/% 9, pch = 0:25, cex = 2, bg = "gold", col = "navy", xlim = c(-1, 9),
       ylim = c(-1, 3), axes = FALSE, xlab = "", ylab = "")
  text(0:25 %% 9, 0:25 %/% 9 - 0.35, 0:25, cex = 0.7)
})
figure("lty_lwd", {
  plot.new()
  plot.window(c(0, 1), c(0, 7))
  for (i in 1:6) segments(0.1, i, 0.9, i, lty = i, lwd = i / 1.5)
  segments(0.1, 6.6, 0.9, 6.6, lty = "44", lwd = 3, lend = "butt")
  box()
})
figure("line_chart", {
  plot(AirPassengers, main = "Air passengers", ylab = "Thousands", col = "steelblue", lwd = 2)
  abline(h = mean(AirPassengers), lty = 2, col = "grey40")
})
figure("multiple_lines", {
  matplot(cumsum(matrix(rnorm(300), 100)), type = "l", lty = 1, lwd = 1.5,
          col = c("firebrick", "darkgreen", "navy"), ylab = "Level", xlab = "Step")
  legend("topleft", c("a", "b", "c"), col = c("firebrick", "darkgreen", "navy"), lty = 1, bty = "n")
})
figure("barplot_simple", barplot(table(mtcars$gear), main = "Gears", col = "tan"))
figure("barplot_beside", {
  counts <- table(mtcars$vs, mtcars$gear)
  barplot(counts, beside = TRUE, col = c("darkblue", "red"), legend.text = c("V", "straight"),
          args.legend = list(x = "topright"), names.arg = paste("gear", colnames(counts)))
})
figure("barplot_stacked_horizontal", {
  par(mar = c(4, 8, 2, 1))
  barplot(VADeaths, horiz = TRUE, las = 1, col = hcl.colors(5, "Teal"), border = NA)
})
figure("barplot_negative", {
  values <- c(-3.2, 1.5, 4.1, -0.7, 2.2)
  centres <- barplot(values, col = ifelse(values < 0, "tomato", "seagreen"), ylim = c(-4, 5))
  text(centres, values + sign(values) * 0.4, format(values))
  abline(h = 0)
})
figure("hist_default", hist(rnorm(500)))
figure("hist_density", {
  data <- c(rnorm(300), rnorm(200, 3, 0.6))
  hist(data, breaks = 30, freq = FALSE, col = "grey85", border = "white", main = "Mixture")
  lines(density(data), lwd = 2, col = "darkred")
  rug(data)
})
figure("hist_breaks_labels", hist(faithful$eruptions, breaks = seq(1.5, 5.5, 0.25), labels = TRUE,
                                   col = "lightblue", ylim = c(0, 50)))
figure("boxplot_formula", boxplot(len ~ supp * dose, data = ToothGrowth, col = c("gold", "darkgreen"),
                                  main = "Tooth growth", notch = FALSE))
figure("boxplot_notch_horizontal", boxplot(count ~ spray, data = InsectSprays, horizontal = TRUE,
                                           notch = TRUE, col = "bisque", las = 1))
figure("boxplot_varwidth", boxplot(split(airquality$Ozone, airquality$Month), varwidth = TRUE,
                                   outline = TRUE, border = "navy", names = month.abb[5:9]))
figure("pie", pie(c(Sky = 78, "Sunny side" = 17, "Shady side" = 5),
                  col = c("#0292D8", "#F7EA39", "#C4B632"), init.angle = -50, border = NA))
figure("dotchart", dotchart(VADeaths, main = "Death rates", cex = 0.8))
figure("stripchart", stripchart(decrease ~ treatment, data = OrchardSprays, method = "jitter",
                                vertical = TRUE, pch = 21, bg = "orange", log = "y"))
figure("pairs_iris", pairs(iris[1:4], pch = 21, bg = c("red", "green3", "blue")[iris$Species],
                           cex = 0.6))
figure("pairs_panels", {
  panel_hist <- function(x, ...) {
    usr <- par("usr")
    par(usr = c(usr[1:2], 0, 1.5))
    h <- hist(x, plot = FALSE)
    rect(h$breaks[-length(h$breaks)], 0, h$breaks[-1], h$counts / max(h$counts), col = "cyan")
  }
  pairs(USJudgeRatings[1:4], diag.panel = panel_hist, lower.panel = panel.smooth, cex.labels = 1.2)
})
figure("coplot", coplot(lat ~ long | depth, data = quakes, number = 4, pch = ".", cex = 2))
figure("image_volcano", image(volcano, col = terrain.colors(40), axes = FALSE, main = "Maunga Whau"))
figure("contour_volcano", {
  image(volcano, col = hcl.colors(30, "YlOrRd", rev = TRUE))
  contour(volcano, add = TRUE, drawlabels = TRUE, levels = seq(90, 200, 10))
})
figure("filled_contour", filled.contour(volcano, color.palette = terrain.colors, asp = 1,
                                        plot.title = title(main = "Volcano")))
figure("persp_surface", {
  y <- x
  z <- outer(x, y, function(a, b) sin(sqrt(a^2 + b^2)))
  persp(x, y, z, theta = 30, phi = 25, col = "lightblue", shade = 0.5, border = NA,
        ticktype = "detailed")
})
figure("heatmap", heatmap(as.matrix(mtcars[1:15, c(1, 3:7)]), scale = "column",
                          col = hcl.colors(20, "Blue-Red")))
figure("mosaicplot", mosaicplot(Titanic, color = TRUE, main = "Titanic"))
figure("spineplot", spineplot(factor(cyl) ~ mpg, data = mtcars))
figure("sunflowerplot", sunflowerplot(iris[, 3:4], seg.col = "darkgreen"))
figure("symbols_circles", symbols(trees$Height, trees$Volume, circles = trees$Girth, inches = 0.25,
                                  bg = "#4682B480", fg = "navy"))
figure("stars", stars(mtcars[1:9, 1:7], key.loc = c(7, 2), draw.segments = TRUE, cex = 0.7))
figure("curve_functions", {
  curve(sin, -pi, 2 * pi, col = "navy", lwd = 2, ylab = "f(x)")
  curve(cos, add = TRUE, col = "firebrick", lwd = 2, lty = 2)
  abline(h = 0, v = 0, col = "grey")
})
figure("regression_line", {
  model <- lm(dist ~ speed, data = cars)
  plot(cars, pch = 16, col = "grey30")
  abline(model, col = "red", lwd = 2)
  grid()
  mtext(sprintf("slope %.2f", coef(model)[2]), side = 3, adj = 1)
})
figure("lm_diagnostics", {
  par(mfrow = c(2, 2), mar = c(4, 4, 2, 1))
  plot(lm(mpg ~ wt + hp, data = mtcars))
})
figure("polygon_shading", {
  plot(x, dnorm(x), type = "n", ylab = "density")
  inside <- x > 1
  polygon(c(1, x[inside], 3), c(0, dnorm(x[inside]), 0), col = "skyblue", border = NA)
  polygon(c(-3, x[x < -1], -1), c(0, dnorm(x[x < -1]), 0), density = 15, angle = 45)
  lines(x, dnorm(x), lwd = 2)
})
figure("rect_arrows_segments", {
  plot(c(0, 10), c(0, 10), type = "n")
  rect(1:4, 1:4, 3:6, 2:5, col = rainbow(4, alpha = 0.6), border = "black", lwd = 2)
  arrows(5, 8, 9, 2, length = 0.15, angle = 20, code = 3, col = "darkred")
  segments(0, 9, 4, 9, lwd = 4, lend = "square")
})
figure("text_fonts", {
  plot.new()
  text(0.5, 0.9, "Plain", font = 1, cex = 1.5)
  text(0.5, 0.75, "Bold", font = 2, cex = 1.5)
  text(0.5, 0.6, "Italic", font = 3, cex = 1.5)
  text(0.5, 0.45, "Bold italic", font = 4, cex = 1.5)
  text(0.2, 0.2, "rotated", srt = 45)
  text(0.8, 0.2, "left adjusted", adj = c(0, 0.5), family = "serif")
  mtext("margin text", side = 1, line = 1, col = "blue")
})
figure("plotmath", {
  plot.new()
  text(0.5, 0.85, expression(hat(beta) == (X^T * X)^{-1} * X^T * y), cex = 1.4)
  text(0.5, 0.6, expression(sum(x[i]^2, i == 1, n)), cex = 1.4)
  text(0.5, 0.35, expression(frac(1, sigma * sqrt(2 * pi)) * e^{-frac((x - mu)^2, 2 * sigma^2)}))
  title(main = expression(paste("Normal density ", phi(x))))
})
figure("legend_variants", {
  plot(1:10, type = "n")
  legend("topleft", c("one", "two"), lty = 1:2, col = 1:2, title = "lines")
  legend("top", c("filled", "hatched"), fill = c("grey", "red"), density = c(NA, 20), horiz = TRUE)
  legend("bottomright", c("a", "b", "c"), pch = 21:23, pt.bg = 2:4, bg = "lightyellow",
         box.lty = 2, ncol = 3)
  legend(2, 5, c("inset text"), bty = "n", text.col = "purple", cex = 1.3)
})
figure("log_axes", plot(1:100, (1:100)^3, log = "xy", type = "l", main = "log-log"))
figure("date_axis", {
  days <- as.Date("2024-01-01") + 0:180
  plot(days, cumsum(rnorm(181)), type = "l", xlab = "", ylab = "value", xaxt = "n")
  axis.Date(1, days, at = seq(days[1], days[181], by = "month"), format = "%b %Y")
})
figure("posixct_axis", {
  times <- as.POSIXct("2024-03-01 09:00", tz = "UTC") + seq(0, 8 * 3600, by = 300)
  plot(times, cumsum(rnorm(length(times))), type = "s", xlab = "time", ylab = "price")
})
figure("custom_axes", {
  plot(1:10, axes = FALSE, xlab = "", ylab = "")
  axis(1, at = c(1, 5, 10), labels = c("low", "mid", "high"), col.axis = "blue", tck = 0.02)
  axis(2, las = 2, cex.axis = 0.8)
  axis(4, at = seq(2, 10, 4), col = "red")
  axis(3, labels = FALSE)
  box(bty = "l")
})
figure("layout_matrix", {
  layout(matrix(c(1, 1, 2, 3), 2, byrow = TRUE), heights = c(2, 1))
  par(mar = c(3, 3, 1, 1))
  plot(faithful)
  hist(faithful$eruptions, main = "")
  hist(faithful$waiting, main = "")
})
figure("outer_margins", {
  par(mfrow = c(1, 2), oma = c(0, 0, 3, 0), mar = c(4, 4, 1, 1), bg = "ivory")
  plot(women)
  plot(rev(women$height), type = "b")
  mtext("Shared title", outer = TRUE, cex = 1.5, font = 2)
})
figure("qqnorm", {
  sample <- rt(200, df = 4)
  qqnorm(sample, pch = 1, frame = FALSE)
  qqline(sample, col = "steelblue", lwd = 2)
})
figure("density_plot", plot(density(precip, bw = "SJ"), main = "Precipitation", lwd = 2))
figure("ecdf_plot", plot(ecdf(rexp(40)), verticals = TRUE, do.points = FALSE, col = "darkorange"))
figure("hclust", plot(hclust(dist(USArrests[1:20, ])), hang = -1, cex = 0.8))
figure("dendrogram_horizontal", plot(as.dendrogram(hclust(dist(scale(mtcars[1:12, ])))), horiz = TRUE))
figure("table_plot", plot(table(rpois(100, 5)), ylab = "count", lwd = 4, col = "navy"))
figure("factor_plot", plot(iris$Species, iris$Sepal.Length, col = "lavender"))
figure("data_frame_plot", plot(airquality[, 1:4], cex = 0.5))
figure("acf_pacf", {
  par(mfrow = c(1, 2))
  acf(lh)
  pacf(lh)
})
figure("ts_decomposition", plot(stl(log(co2), s.window = "periodic")))
figure("multiple_ts", plot(EuStockMarkets, main = "European indices"))
figure("cdplot", cdplot(factor(am) ~ mpg, data = mtcars))
figure("fourfoldplot", fourfoldplot(UCBAdmissions[, , 1:4], mfrow = c(2, 2)))
figure("assocplot", assocplot(HairEyeColor[, , "Female"]))
figure("transparency", {
  plot(rnorm(2000), rnorm(2000), pch = 16, col = rgb(0, 0, 1, 0.15), cex = 1.5)
  points(0, 0, pch = 4, cex = 3, lwd = 3, col = "red")
})
figure("raster_image", {
  plot(c(0, 1), c(0, 1), type = "n")
  rasterImage(as.raster(matrix(hcl.colors(12), 3, 4)), 0.1, 0.1, 0.6, 0.6, interpolate = FALSE)
  rasterImage(as.raster(matrix(gray(0:9 / 9), 1)), 0.65, 0.2, 0.95, 0.9)
})
figure("clip_region", {
  plot(1:10, type = "n")
  clip(3, 7, 3, 7)
  abline(a = 0, b = 1, col = "red", lwd = 3)
  abline(h = 5, col = "blue", lwd = 3)
})
figure("xspline_shapes", {
  plot(c(0, 5), c(0, 5), type = "n", asp = 1)
  xspline(c(1, 2, 3, 4), c(1, 4, 1, 4), shape = 1, lwd = 2, border = "purple")
  xspline(c(1, 2, 3), c(2, 3, 2), shape = -1, open = FALSE, col = "pink")
})
figure("grid_background", {
  plot(pressure, type = "n", log = "y")
  grid(col = "grey80", lty = 1)
  lines(pressure, lwd = 2)
  points(pressure, pch = 21, bg = "white")
})
figure("error_bars", {
  means <- tapply(warpbreaks$breaks, warpbreaks$tension, mean)
  errors <- tapply(warpbreaks$breaks, warpbreaks$tension, sd) / sqrt(18)
  centres <- barplot(means, ylim = c(0, 45), col = "grey80")
  arrows(centres, means - errors, centres, means + errors, angle = 90, code = 3, length = 0.08)
})
figure("unicode_labels", {
  plot(1:5, main = "Temperatur °C – Messwerte", xlab = "α β γ",
       ylab = "été")
})
figure("big_scatter", plot(rnorm(20000), rnorm(20000), pch = ".", col = "grey20"))
figure("matplot_points", matplot(iris[, 1:2], iris[, 3:4], pch = c(1, 3), col = c("blue", "red")))
figure("smooth_lowess", {
  plot(cars, main = "lowess")
  lines(lowess(cars), col = "red", lwd = 2)
  lines(lowess(cars, f = 0.2), col = "blue", lty = 2)
})
figure("bxp_stats", {
  stats <- boxplot(count ~ spray, data = InsectSprays, plot = FALSE)
  bxp(stats, boxfill = "lightgreen", whisklty = 1, staplewex = 0.8, medcol = "red")
})
figure("par_settings", {
  par(cex = 0.8, col.axis = "darkgrey", col.lab = "navy", fg = "grey30", tcl = -0.3, mgp = c(2, 0.5, 0),
      las = 1, family = "mono")
  plot(rivers, main = "par() settings", xlab = "index")
})
figure("abline_variants", {
  plot(0:10, 0:10, type = "n")
  abline(h = 1:9, col = "grey90")
  abline(v = c(2, 8), lty = 3)
  abline(a = 1, b = 0.5, lwd = 2, col = "orange")
  abline(coef = c(10, -1), col = "purple")
})
