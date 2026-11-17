# deparse() writes a complex element as `complex(real=, imaginary=)` when its real part is NaN
# or NA or its imaginary part is not finite, and as a literal such as `Inf+0i` when only the
# real part is infinite. With an NA in either part, the other part is rounded to an integer.
# An element NA in both parts is `NA_complex_` only when every element is NA.
parts <- c(0, 1.5, -2, 2.5, 0.25, Inf, -Inf, NaN, NA)
for (r in parts) for (i in parts) {
  z <- complex(real = r, imaginary = i)
  cat(sprintf("%-5s %-5s", r, i), "|", deparse(z), "|", deparse(z, control = "digits17"), "|",
    deparse(z, control = "all"), "|", deparse(c(z, 1i)), "|", deparse(c(a = z)), "\n")
}
dput(c(complex(real = Inf, imaginary = 0), NA, complex(real = NA, imaginary = 1)))
dput(list(complex(real = Inf, imaginary = 0)))
dput(complex(real = NA, imaginary = NA))
dput(c(NA_complex_, NA_complex_))
dput(complex(real = NaN, imaginary = NaN), control = "all")
deparse(complex(real = 1e-20, imaginary = Inf))
deparse(complex(real = 0.1, imaginary = -Inf), control = "digits17")
deparse(complex(real = 1/3, imaginary = Inf))
deparse(complex(real = -Inf, imaginary = 1/3), control = "digits17")
deparse(complex(real = 123456789012, imaginary = NaN))
deparse(complex(real = 123456.75, imaginary = NA))
deparse(complex(real = -0.5, imaginary = NA))
z <- c(complex(real = Inf, imaginary = 0), 1+2i, complex(real = 0, imaginary = Inf))
identical(eval(parse(text = deparse(z))), z)
