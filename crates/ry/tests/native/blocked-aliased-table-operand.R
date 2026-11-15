# blocked: direct
# An operator applied to a call whose result type is an alias of a table type.
alias Rule = Expr
alias Sales = DataFrame{units: integer[], price: double[]}
sales <- DataFrame(units = c(1L, 2L, 3L), price = c(10, 20, 30))
units <- function(name: Str) -> Rule Expr.col(name)
direct <- function(sales: Sales) -> F64 {
  kept <- DataFrame.filter(sales, units("units") > 2L)
  List.sum(List.from_r(kept$price))
}
through_a_variable <- function(sales: Sales) -> F64 {
  rule <- units("units")
  kept <- DataFrame.filter(sales, rule > 2L)
  List.sum(List.from_r(kept$price))
}
cat(direct(sales), through_a_variable(sales), "\n")
cat(direct(sales), through_a_variable(sales), "\n")
