# expect: ok
# An expression of literals adapts to the type its context asks for, like a single literal does.
circumference <- function(r: F64) -> F64 2 * 3.14159 * r
print(circumference(1))
#> [1] 6.28318
