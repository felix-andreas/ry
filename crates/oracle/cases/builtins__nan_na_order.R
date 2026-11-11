# When NaN meets NA in `+`, `-`, `*` and `/`, GNU R keeps the first operand's NaN, which is the
# x86 rule once an integer or logical NA has become NA_real_: `NaN op NA` is NaN and `NA op NaN`
# is NA, for an integer, logical and double NA alike, scalar and vector. GNU R's interpreter
# leaves the order of a scalar `+` or `*` to the C compiler that built it, whatever the NA, so
# `scalar` is compiled before its first call and every call takes the byte-code path.
scalar <- compiler::cmpfun(function(a, b) c(a + b, a - b, a * b, a / b))
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
