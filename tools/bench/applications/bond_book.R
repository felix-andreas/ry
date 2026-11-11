# Typed code calling base R: price a book of bonds, each a short vector computation of base R
# functions (seq_len, rep, exp, sum).
bond_price <- function(coupon, rate, years) {
  times <- seq_len(years)
  flows <- rep(coupon, years)
  flows[[years]] <- flows[[years]] + 100
  sum(flows * exp(-rate * times))
}
price_book <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    total <- total + bond_price(2 + i %% 5L, 0.01 + (i %% 7L) * 0.003, 5L + i %% 20L)
  }
  round(total, 6)
}
measure("bond_book", function() price_book(200000L))
