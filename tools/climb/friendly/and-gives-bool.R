# expect: ok
# `&&` on two Bools gives a Bool.
both: Bool <- (1.5f64 > 1) && (2i64 < 3)
print(both)
#> [1] TRUE
