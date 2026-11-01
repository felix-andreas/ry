# Complex trigonometric and hyperbolic functions, and how complex vectors print: digits are
# chosen after rounding each value relative to its larger part, so a tiny part prints as zero.
z <- c(1+2i, -0.5+0.3i, 2+0i, -3+0i, 0+3i, 0-2i, 0.2+30i)
for (f in list(sqrt, exp, cos, sin, tan, acos, asin, atan, cosh, sinh, tanh, acosh, asinh, atanh)) print(f(z))
print(complex(real = 1, imaginary = -0))
print(complex(real = 1, imaginary = -1e-20))
print(c(1-1e-20i, 2+3i))
print(0.03381283+1.0147936i)
print(-1e-20+1i)
format(1-1e-20i)
as.character(1-1e-20i)
