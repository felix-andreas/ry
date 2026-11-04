# Negative subscripts on plain vectors take the masked fast path: zeros, fractions, duplicates,
# positions past the end and a mix with a positive subscript.
x <- c(10, 20, 30, 40, 50); l <- list(1, "a", TRUE); s <- letters[1:5]
print(x[-(1:2)]); print(x[c(-1, 0)]); print(x[-c(5, 100)]); print(x[-0.5]); print(x[c(0, 0)])
print(x[-1.9]); print(s[-3L]); str(l[-2]); print(x[c(-1L, -1L)]); print(integer(0)[-1])
print(tryCatch(x[c(-1, 2)], error = conditionMessage)); print(x[-6:-1])
