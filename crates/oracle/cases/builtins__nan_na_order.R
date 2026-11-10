# When NaN meets NA in `+`, `-`, `*` and `/`, GNU R keeps the first operand's NaN, which is the
# x86 rule once an integer or logical NA has become NA_real_: `NaN op NA` is NaN and `NA op NaN`
# is NA, for an integer, logical and double NA alike, scalar and vector. The first call runs
# before GNU R compiles `scalar`, and there the build's C compiler decides `NaN + NA_integer_`,
# so that call uses NA_real_, whose order every build keeps.
scalar <- function(a, b) c(a + b, a - b, a * b, a / b)
for (na in list(NA_real_, NA_integer_, NA)) {
  print(scalar(NaN, na))
  print(scalar(na, NaN))
  print(c(NaN + na, NaN - na, NaN * na, NaN / na))
  print(c(na + NaN, na - NaN, na * NaN, na / NaN))
  for (op in c("+", "-", "*", "/")) {
    f <- match.fun(op)
    print(f(c(NaN, NaN), c(na, na)))
    print(f(c(na, na), c(NaN, NaN)))
  }
}
for (k in 1:3) print(scalar(NaN, NA))
is.nan(NaN + NA)
