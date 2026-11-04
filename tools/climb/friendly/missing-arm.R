# expect: non-exhaustive-match
# A match must handle every tag.
alias Shape = :Circle(radius: F64) | :Square(side: F64)
area <- function(s: Shape) -> F64 match (s) {
  :Circle(radius = r) => r * r * 3.14159
}
print(area(:Square(side = 2)))
