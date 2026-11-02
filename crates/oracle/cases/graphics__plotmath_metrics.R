# The width and height in inches of plotmath expressions, laid out by `GEExpressionWidth` and
# `GEExpressionHeight` with the PDF device's font metrics.
pdf(NULL)
plot.new()
exprs <- expression(
  x, x * y, x[i], x^2, x[i]^2, x[ij]^{k + 1}, x^y^z,
  sqrt(x), sqrt(x, 3), root(x^2, n),
  frac(x, y), over(1, x + y), atop(a, b), frac(frac(a, b), c),
  hat(x), tilde(y), bar(xy), widehat(xy), widetilde(xy), underline(x),
  plain(x), italic(x), bold(x), bolditalic(x), paste("a ", x, " b"),
  displaystyle(frac(a, b)), textstyle(frac(a, b)), scriptstyle(x), scriptscriptstyle(x),
  phantom(x), x * phantom(0) * y, vphantom(x), abs(x), f(x), "text", 3.14159265, 12L,
  x + y, x - y, x / y, x %+-% y, -x, x == y, x <= y, x %->% y, x %in% y,
  alpha + beta, Omega, infinity, x ~ y,
  sum(x[i], i == 1, n), prod(x[i], i == 1, n), integral(f(x) * dx, a, b), lim(f(x), x %->% 0),
  group("(", x, ")"), bgroup("(", frac(a, b), ")"), bgroup("{", atop(x, y), "}"),
  list(x, y, z), x[1] + ldots + x[n], x[1] + cdots + x[n],
  hat(beta) == (X^T * X)^{-1} * X^T * y
)
for (i in seq_along(exprs)) {
  cat(sprintf("%2d %8.4f %8.4f\n", i,
              round(strwidth(exprs[i], units = "inches"), 4),
              round(strheight(exprs[i], units = "inches"), 4)))
}
par(cex = 1.5)
round(strwidth(expression(sum(x[i]^2, i == 1, n)), units = "inches"), 4)
round(strheight(expression(sum(x[i]^2, i == 1, n)), units = "inches"), 4)
invisible(dev.off())
