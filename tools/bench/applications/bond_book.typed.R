# Price a book of bonds, each a short computation over its cash flows: a coupon every year and the
# principal with the last one, discounted continuously.
bond_price <- function(coupon: F64, rate: F64, years: I64) -> F64 {
  List.walk(List.range(1, years), 0, function(total, year) {
    flow <- if (year == years) coupon + 100 else coupon
    total + flow * Num.pow(Num.e, -rate * Num.to_f64(year))
  })
}
price_book <- function(n: I64) -> F64 {
  total <- 0
  for (i in List.range(1, n)) {
    total <- total + bond_price(Num.to_f64(2 + i %% 5), 0.01 + Num.to_f64(i %% 7) * 0.003, 5 + i %% 20)
  }
  Num.to_f64(Num.round(total * 1000000)) / 1000000
}
measure("bond_book", function() price_book(200000))
