# oracle: library-dependent
# Complex sums where one operand's part is NA and the other's is NaN. GNU R adds the parts in C,
# where the result is NA or NaN depending on which operand the compiler put first, so only the
# recorded build confirms these. The operands are those of kernels__complex.R.
z <- c(complex(real = 1.5, imaginary = -2), NA, complex(real = NaN, imaginary = 1),
  complex(real = 1, imaginary = NaN), complex(real = Inf, imaginary = 0),
  complex(real = 0, imaginary = -Inf), complex(real = Inf, imaginary = Inf), 0, -3, 2i)
d <- c(NA, NaN, 2, -1, Inf, 0, 0.5, -Inf, 3, 1)
pairs <- list(zd = list(z, d), dz = list(d, z), zna = list(z, NA), nas = list(NA_complex_, z))
# Called through a variable, as kernels__complex.R calls it, so the byte compiler leaves the call.
plus <- `+`
for (pair in names(pairs)) {
  cat("+", pair, ":\n")
  sum <- plus(pairs[[pair]][[1]], pairs[[pair]][[2]])
  print(rbind(re = Re(sum), im = Im(sum)))
}
