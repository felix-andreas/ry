# expect: ok
# A literal in `return()` takes the declared result type, as a tail literal does.
pick <- function(b: Bool) -> I64 {
  if (b) return(4)
  5
}
print(pick(TRUE))
#> 4i64
