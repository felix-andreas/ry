# blocked: by_position, prefixed, boxed
# Table forms the typed core does not compile.
alias Sales = DataFrame{units: integer[], price: double[]}
sales <- DataFrame(units = c(1L, 2L, 3L), price = c(10, 20, 30))
by_position <- function(sales: Sales) -> F64 {
  List.sum(List.from_r(sales[[2]]))
}
prefixed <- function(sales: Sales) -> F64 {
  big <- DataFrame.filter(sales, ry::Expr.col("units") > 2L)
  List.sum(List.from_r(big$price))
}
boxed <- function(sales: Sales) -> F64 {
  held <- (rule = Expr.col("units") > 2L, count = 1)
  big <- DataFrame.filter(sales, held$rule)
  List.sum(List.from_r(big$price))
}
cat(by_position(sales), prefixed(sales), boxed(sales), "\n")
