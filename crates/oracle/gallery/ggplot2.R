# oracle: requires ggplot2
# ggplot2 as data science code uses it: the common geoms, statistics, facets, coordinate
# systems, scales and themes.
suppressPackageStartupMessages(library(ggplot2))

figure("gg_point_colour", print(ggplot(mpg, aes(displ, hwy, colour = class)) + geom_point()))
figure("gg_point_size_alpha", print(ggplot(diamonds[1:2000, ], aes(carat, price, size = depth)) +
                                      geom_point(alpha = 0.3, colour = "navy")))
figure("gg_line", print(ggplot(economics, aes(date, unemploy)) + geom_line(colour = "firebrick")))
figure("gg_bar_count", print(ggplot(mpg, aes(class)) + geom_bar(fill = "steelblue")))
figure("gg_col_dodge", print(ggplot(mtcars, aes(factor(cyl), mpg, fill = factor(gear))) +
                               stat_summary(fun = mean, geom = "col", position = "dodge")))
figure("gg_histogram", print(ggplot(diamonds, aes(price)) + geom_histogram(bins = 40)))
figure("gg_density_fill", print(ggplot(iris, aes(Sepal.Length, fill = Species)) +
                                  geom_density(alpha = 0.4)))
figure("gg_boxplot", print(ggplot(mpg, aes(class, hwy)) + geom_boxplot(outlier.colour = "red") +
                             coord_flip()))
figure("gg_violin", print(ggplot(ToothGrowth, aes(factor(dose), len)) + geom_violin(fill = "plum") +
                            geom_jitter(width = 0.1, height = 0)))
figure("gg_smooth_lm", print(ggplot(mtcars, aes(wt, mpg)) + geom_point() +
                               geom_smooth(method = "lm", formula = y ~ x)))
figure("gg_smooth_loess", print(ggplot(mpg, aes(displ, hwy)) + geom_point() +
                                  geom_smooth(method = "loess", formula = y ~ x, span = 0.7)))
figure("gg_tile", print(ggplot(faithfuld, aes(waiting, eruptions, fill = density)) + geom_raster() +
                          scale_fill_viridis_c()))
figure("gg_facet_wrap", print(ggplot(mpg, aes(displ, hwy)) + geom_point() + facet_wrap(~class, nrow = 2)))
figure("gg_facet_grid", print(ggplot(mpg, aes(cty, hwy)) + geom_point() + facet_grid(drv ~ cyl)))
figure("gg_polar_pie", print(ggplot(as.data.frame(table(mpg$drv)), aes("", Freq, fill = Var1)) +
                               geom_col(width = 1) + coord_polar(theta = "y") + theme_void()))
figure("gg_theme_minimal", print(ggplot(mtcars, aes(hp, qsec)) + geom_point() + theme_minimal() +
                                   labs(title = "Minimal", subtitle = "theme_minimal()",
                                        caption = "mtcars")))
figure("gg_theme_bw_legend_bottom", print(ggplot(iris, aes(Petal.Length, Petal.Width,
                                                           shape = Species)) +
                                            geom_point() + theme_bw() +
                                            theme(legend.position = "bottom")))
figure("gg_scale_log", print(ggplot(msleep, aes(bodywt, brainwt)) + geom_point(na.rm = TRUE) +
                               scale_x_log10() + scale_y_log10()))
figure("gg_text_annotate", print(ggplot(mtcars[1:10, ], aes(wt, mpg, label = rownames(mtcars)[1:10])) +
                                   geom_text(check_overlap = TRUE) +
                                   annotate("rect", xmin = 2, xmax = 3, ymin = 20, ymax = 25,
                                            alpha = 0.2, fill = "blue")))
figure("gg_area_stacked", print(ggplot(data.frame(x = rep(1:20, 3), y = abs(rnorm(60)),
                                                  g = rep(c("a", "b", "c"), each = 20)),
                                       aes(x, y, fill = g)) + geom_area()))
figure("gg_ribbon", print(ggplot(data.frame(x = 1:50, y = cumsum(rnorm(50))),
                                 aes(x, y)) + geom_ribbon(aes(ymin = y - 1, ymax = y + 1),
                                                          fill = "grey70") + geom_line()))
figure("gg_errorbar", print(ggplot(data.frame(group = c("a", "b", "c"), mean = c(3, 5, 4),
                                              se = c(0.4, 0.6, 0.3)),
                                   aes(group, mean)) + geom_point(size = 3) +
                              geom_errorbar(aes(ymin = mean - se, ymax = mean + se), width = 0.2)))
figure("gg_ecdf_step", print(ggplot(data.frame(x = c(rnorm(100), rnorm(100, 1)),
                                               g = rep(c("a", "b"), each = 100)),
                                    aes(x, colour = g)) + stat_ecdf(geom = "step")))
figure("gg_hline_vline_labels", print(ggplot(mtcars, aes(wt, mpg)) + geom_point() +
                                        geom_hline(yintercept = 20, linetype = "dashed") +
                                        geom_vline(xintercept = 3.5, colour = "red") +
                                        xlab("weight") + ylab("mpg") + ggtitle("Reference lines")))
figure("gg_manual_colours", print(ggplot(mpg, aes(displ, hwy, colour = drv)) + geom_point() +
                                    scale_colour_manual(values = c("4" = "darkgreen", f = "orange",
                                                                   r = "purple"))))
