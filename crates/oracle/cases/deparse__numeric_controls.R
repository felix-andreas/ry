# Numbers under the hexNumeric and digits17 controls: a finite complex is written part by
# part, and a value that is not finite is written as without the control.
zs <- list(1+2i, c(1.5-2.25i, 0i, NA, -1i), complex(real = NaN, imaginary = 1), complex(real = Inf, imaginary = -Inf), complex(real = NA, imaginary = 1), complex(real = 1, imaginary = NA), 0.1+0.3i, -0.1-1e-20i, 1e300+1e-300i, complex(real = -0, imaginary = -0), c(a = 1i, b = 2), 3+0i, complex(real = 1/3, imaginary = 2/3), complex(0), NA_complex_, c(1i, NaN), 123456789.123456789+1i, 1e15+1e15i, 1e5+1e-5i, 100000+1i, 1e-5i, complex(real=NaN, imaginary=NaN), complex(real = 2^53, imaginary = 2^-1074))
for (z in zs) for (ctl in list("keepNA", "all", NULL, "hexNumeric", "digits17", "exact", c("keepInteger", "hexNumeric"), c("all", "hexNumeric"), c("all", "digits17"))) {
  cat(deparse(z, control = ctl), sep = "\n")
}
dput(zs, control = "exact"); dput(zs, control = c("all", "digits17")); dput(zs)
for (ctl in list("hexNumeric", "digits17", "exact")) { cat(deparse(quote(x + 1i), control = ctl), deparse(call("f", 1+2i, 0.1), control = ctl), deparse(c(0.1, 1/3, 1e-300, NA, Inf, -0, 2^60, 5e-324, 100, 1e5, 123456.7), control = ctl), deparse(list(1L, 2.5, TRUE, "a", NULL, 3i), control = ctl), sep = "\n") }
