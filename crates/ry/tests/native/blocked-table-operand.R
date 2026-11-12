# blocked: picked
# An operator tells a table expression among its operands by the operand's form. `named` passes
# the same expression through a variable, and `helped` through a top-level function, which both
# compile.
units <- function() -> Expr Expr.col("units")
picked <- function(sales: DataFrame{units: integer[], price: double[]}, wide: Bool) -> DataFrame {
  DataFrame.filter(sales, (if (wide) Expr.col("units") else Expr.col("price")) > 3L)
}
named <- function(sales: DataFrame{units: integer[], price: double[]}, wide: Bool) -> DataFrame {
  column <- if (wide) Expr.col("units") else Expr.col("price")
  DataFrame.filter(sales, column > 3L)
}
helped <- function(sales: DataFrame{units: integer[], price: double[]}) -> DataFrame {
  DataFrame.filter(sales, units() > 3L)
}
sales <- DataFrame(units = c(1L, 5L, 9L), price = c(1.5, 2.5, 3.5))
print(DataFrame.height(picked(sales, TRUE)))
print(DataFrame.height(named(sales, FALSE)))
print(DataFrame.height(helped(sales)))
