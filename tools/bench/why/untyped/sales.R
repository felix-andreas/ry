# A table pipeline: filter a million sales rows, add a revenue column, and total it per region.
pipeline <- function(sales) {
  big <- sales[sales$units > 3L, ]
  big$revenue <- big$units * big$price
  totals <- tapply(big$revenue, big$region, sum)
  sum(totals * seq_along(totals))
}
i <- seq_len(1000000L)
sales <- data.frame(region = c("north", "south", "east", "west")[i %% 4L + 1L], units = i %% 17L, price = 1 + i %% 100L)
measure("sales", function() pipeline(sales))
