# A product already NaN from Inf * 0 keeps its NaN against a later NA; the first NaN input wins
# otherwise. Shifts outside 0..31 are NA.
print(prod(c(Inf, 0, NA)))
print(prod(c(NA, Inf, 0)))
print(prod(c(Inf, 0, NaN)))
print(prod(c(NaN, NA)))
print(prod(c(NA, NaN)))
print(is.na(prod(c(1, NA, 2))))
print(prod(c(Inf, 0), NA))
print(bitwShiftL(1L, c(-1L, 0L, 1L, 30L, 31L, 32L, 33L, NA)))
print(bitwShiftR(-1L, c(-1L, 0L, 1L, 31L, 32L)))
print(bitwShiftL(-1L, 31L))
print(bitwShiftL(c(1L, 2L, 3L), 2L))
