# A table column of tags without a payload: typed code builds it from a `List` of tags, which
# reaches the function from R as a ry list, and compares the column with a tag.
alias Rows = DataFrame{kind: :Bond | :Cash, v: double}
build <- function(kinds: List<:Bond | :Cash>) -> Rows {
  DataFrame(kind = kinds, v = c(1, 2))
}
bonds <- function(rows: Rows) -> Rows {
  DataFrame.filter(rows, Expr.col("kind") == :Bond)
}
counted <- function(kinds: List<:Bond | :Cash>) -> I64 {
  List.len(kinds)
}
rows <- build([:Bond, :Cash])
print(bonds(rows))
print(counted([:Bond, :Cash, :Bond]))
