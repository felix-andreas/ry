# blocked: revenue
# A column read gives an R vector.
revenue <- function(sales: DataFrame{units: integer[], price: double[]}) -> F64 {
  price <- sales$price
  List.sum(List.from_r(price))
}
sales <- DataFrame(units = c(1L, 2L), price = c(2.5, 4))
cat(revenue(sales), "\n")
