# sprintf: every conversion with flags, widths and precisions, `*` and `n$` references, NA and
# the non-finite values in each conversion, recycling, and the errors and warnings, alone and
# vectorized, so a scalar fast path and the general path give the same text.
p <- function(...) print(sprintf(...))
e <- function(...) print(tryCatch(sprintf(...), error = conditionMessage))
x <- c(0, -0, 1.25, -1.25, 0.125, 2.5, 3.5, 1e15, 123456.789, 1e-5, 9.9999995, 0.1 + 0.2, 1/3, 2^60, 1.7976931348623157e308, 5e-324)
for (f in c("%f", "%.0f", "%.2f", "%.15f", "%.20f", "%10.3f", "%-10.3f|", "%+.1f", "% .1f", "%010.2f", "%#.0f", "%-+08.2f|")) p(f, x)
for (f in c("%e", "%.0e", "%.3e", "%E", "%12.4e", "%-12.2e|", "%+e", "%#.0e", "%010.1e")) p(f, x)
for (f in c("%g", "%.0g", "%.1g", "%.3g", "%.17g", "%G", "%#g", "%#.3g", "%10g", "%-10g|", "%+g", "%010g")) p(f, x)
# With `#`, glibc prints `1.e+02` when rounding carries a value just below 10^precision up to it.
g <- c(9.9996, 99.996, 999.96, 9999.6, 0.099996, 99.95, 999999.5, 9999999.5, -99.95, 95, 1e100 * 0.9999999999)
for (f in c("%#.3g", "%#.2g", "%#.1g", "%#g", "%#.7g", "%#.3G", "%#.0g", "%#10.2g|")) p(f, g)
print(formatC(c(99.95, 999.96, 9.9996), digits = 2, format = "g", flag = "#")); print(formatC(999999.5, format = "g", flag = "#"))
for (f in c("%a", "%.3a", "%A")) p(f, c(1, -0.1, 3e-310))
# Scalars one at a time, the shape a loop calls sprintf with.
for (v in x) print(c(sprintf("%.2f", v), sprintf("%e", v), sprintf("%g", v), sprintf("%s", v)))
for (v in c(1.005, 2.675, 0.045, 1.115, 8.345, 1234.5675)) print(c(sprintf("%.2f", v), sprintf("%.3f", v)))
# Specials in every numeric conversion keep the width and drop the precision.
y <- c(NA, NaN, Inf, -Inf)
for (f in c("%f", "%5.1f", "%-6.2e|", "%+g", "% g", "%08.3f", "%s", "%d")) e(f, y)
# Integers and logicals.
i <- c(0L, 7L, -7L, 255L, NA, .Machine$integer.max, -.Machine$integer.max)
for (f in c("%d", "%i", "%5d", "%-5d|", "%05d", "%+d", "% d", "%.3d", "%.0d", "%x", "%X", "%#x", "%o", "%#o", "%8.4x", "%s", "%f")) e(f, i)
for (f in c("%#.14o", "%#.3o", "%#.0o", "%#5.0o|", "%#08o", "%#-8o|", "%#o")) p(f, c(0L, 8L, -78555L, 511L))
p("%d %s %5i", c(TRUE, FALSE, NA), c(TRUE, FALSE, NA), NA)
p("%d", c(1, 2, NA, -3)); p("%5.2d", 3); p("%x", 255); p("%o", 8)
# Strings: width and precision count bytes of whole characters.
s <- c("a", "", NA, "héllo", "ab cd")
for (f in c("%s", "%5s", "%-5s|", "%.2s", "%5.1s|", "%.0s|")) p(f, s)
p("%s", list(1, "a", TRUE)); p("%s", factor(c("u", "v"))); p("%s and %s", 1.5, 1e-20); p("%s", 123456789012)
p("%s", 1e15); p("%s", 0.1 + 0.2); p("%5s|%-5s", 1L, 2.5)
# `*` widths and precisions, `n$` references and `%%`.
p("%*d|", 5, 42L); p("%-*d|", 5, 42L); p("%*d|", -5, 42L); p("%.*f", 2L, pi); e("%*.*f|", 9, 3, pi)
p("%2$s %1$s", "a", "b"); p("%1$s %1$s", "x"); p("%2$*1$d|", 6, 7L); p("100%% of %s", "it"); p("%%")
# A negative `*` precision is spliced into the specification, which glibc echoes unparsed.
for (f in c("%.*s|", "%5.*s|", "%-+ #0.*f|", "% -.*d|", "%0+.*e|", "%#-12.*g|", "%007.*x|", "%.*i|")) e(f, -3L, if (grepl("[dix]", f)) 7L else if (grepl("s", f)) "a" else 2.5)
p("%.*f", c(-1L, 2L), 1.5); p("%.*d", -1L, NA_integer_); p("%5.*f|", -1L, c(NA, Inf)); p("%.*s", -1, NA); p("%.*f", -1L, TRUE)
e("%5%|"); p("no conversions"); p(c("%d", "<%d>", "[%d]"), 1:6); p("%s-%d", c("a", "b"), 1:4)
# Vectorized formats and recycling with lengths that do not divide.
p(c("%.1f", "%.3e"), c(pi, exp(1), 1/7, 2)); p("%d:%s", 1:3, "z")
e("%s %s", 1:2, 1:3); e(c("%d", "%d", "%d"), 1:2)
# Zero-length arguments and formats.
p("%d", integer()); p(character(), 1); p("%s", NULL)
# Errors.
e("%d", 1.5); e("%d", "a"); e("%f", "a"); e("%s %s", "a"); e("%3$s", "a", "b"); e("%y", 1)
e("%*d", NA_integer_, 1L); e("%s", quote(x)); e("%s", as.name("y")); e("%"); e(1, 2); e("%l", 1L)
e("%.2f", TRUE); e("%s", .Machine$integer.max + 1); print(tryCatch(do.call(sprintf, c(list("%s"), as.list(1:101))), error = conditionMessage))
print(nchar(do.call(sprintf, c(list(strrep("%s", 99)), as.list(1:99)))))
# Type errors show the specification as GNU R runs it: no `n$`, and the `*` value written in.
e("%*d", -5L, "a"); e("%.*d", 3L, "a"); e("%2$*1$d", 4L, "a"); e("%-*.3d|", 7L, 1.5); e("%*d", c(1L, 2L), c(1L, NA, 3L, 4.5)); e("%.*f", -2L, "a"); e("%*1$d", 4L)
# `%d` takes a double only when every element is whole and in range, or NA; NaN is not NA here.
e("%d", NaN); e("%d", Inf); e("%d", c(1, NaN)); e("%d", c(NA, 1)); e("%x", NaN); e("%d", c(1, 2^31)); e("%d", c(NaN, NA))
# The unused-argument warnings, and the one -2^31 gives as it becomes NA.
f <- function(...) withCallingHandlers(sprintf(...), warning = function(w) { print(conditionMessage(w)); invokeRestart("muffleWarning") })
print(f("%s", 1, 2)); print(f("x", 1)); print(f("%2$s", 1, 2)); print(f(c("%s", "%s"), 1, 2, 3))
print(f("%d", c(-2^31, -2^31, 5))); print(f("%d %d", -2^31, c(1, 2, 3))); print(f(c("%d", "%d"), c(-2^31, 4)))
