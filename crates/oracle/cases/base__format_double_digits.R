# format() of doubles: the digit counts GNU R's extended-precision scaling gives, rounding that
# widens a value in scientific but not in fixed notation, the exponent width, and how the
# scientific, scipen and nsmall settings choose between the two notations.
p <- function(...) print(format(...))
# Specials, alone and mixed with finite values; negative zero prints as zero.
p(c(NA, NaN, Inf, -Inf)); p(c(1.5, NA, -Inf)); p(c(NaN, 1e-20)); p(-0); p(c(-0, -1.5)); p(c(-0, 1e-300))
p(c(NA, 1), na.encode = FALSE); p(NA_real_, width = 6); p(c(Inf, 1e300), scientific = FALSE)
# At 15 digits GNU R scales in long double, so these keep fewer or more digits than the
# correctly rounded decimal would.
x <- c(4.0270798167452e-09, 6.247001711546e-11, 6.54224457571570e-11, 2.53554433569600e-11)
for (v in x) p(v, digits = 15); p(x, digits = 15); p(x * 1e20, digits = 15)
p(0.1 + 0.2, digits = 15); p(0.1 + 0.2, digits = 16); p(0.1 + 0.2, digits = 17); p(1/3, digits = 22)
for (d in 1:22) p(c(pi, -exp(1) * 1e5, 1/7e8), digits = d)
for (d in c(1L, 7L, 15L)) p(c(9.9999999, 99999.95, 0.000999999), digits = d)
p(tryCatch(format(pi, digits = 0), error = conditionMessage)); p(tryCatch(format(pi, digits = 23), error = conditionMessage))
# Rounding that carries into a new digit: scientific shows it, fixed notation does not.
p(999999999999999); p(-969.87, digits = 1); p(95.4, digits = 1); p(963331, digits = 1); p(9996, digits = 3)
p(c(9996, 1), digits = 3); p(99999.5, digits = 5); p(0.99996, digits = 4)
# The edges of the double range, where GNU R scales by glibc's powl.
p(5e-324); p(5e-324, digits = 15); p(2.2250738585072014e-308, digits = 15); p(1.7976931348623157e308, digits = 15)
p(c(1e-300, 1e300)); p(1e-310, digits = 3); p(c(1e28, 1e-28), digits = 15); p(123456789e300, digits = 9)
p(c(1e100, 1)); p(c(1e99, 1)); p(c(1e-99, 1)); p(c(1e-100, 1)); p(9.9999e99, digits = 3)
# Mixed magnitudes force scientific notation for the whole vector.
p(c(1e-10, 1, 1e10)); p(c(123456, 0.001234)); p(c(1, 1e15)); p(c(1, 1e16)); p(c(0.1, 123456789012))
# scientific = TRUE and FALSE are penalties of -99 and 310, not absolute choices.
p(1e300, scientific = FALSE); p(1e-300, scientific = FALSE); p(1e-200, scientific = FALSE); p(123, scientific = TRUE)
p(c(1e-10, 1), scientific = FALSE); p(1e5, scientific = 3L); p(1e5, scientific = -3L); p(0, scientific = TRUE)
# options(scipen) applies unless scientific is given; nsmall widens only fixed notation.
op <- options(scipen = 5); p(1e10); p(1e-10); p(c(1e-10, 1), scientific = NA); options(op)
op <- options(scipen = -5); p(100); p(c(1, 10)); p(0); options(op)
p(1e10, nsmall = 2); p(c(1, 1e-10), nsmall = 3); p(123.456, nsmall = 5, digits = 2); p(1e5, nsmall = 20)
# The decimal mark, from the argument and from options(OutDec).
p(c(1.5, -2.25), decimal.mark = ","); p(1e-10, decimal.mark = ","); op <- options(OutDec = ","); p(c(1.5, 1e-20)); p(pi, digits = 3, nsmall = 4); options(op)
p(c(1.5, NA, 3), width = 10, trim = TRUE); p(c(1.5, 22.25), trim = TRUE)
