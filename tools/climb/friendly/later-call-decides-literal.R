# expect: ok
# A literal binding takes its type from the first call that uses it.
half <- function(x: F64) -> F64 x / 2
main <- function() -> F64 {
  rate <- 2.0
  half(rate)
}
print(main())
#> [1] 1
