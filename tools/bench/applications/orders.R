# Business logic over records: price each order line with a loyalty discount and a regional tax,
# then total the invoices.
price_line <- function(order, rates) {
  net <- order$quantity * order$unit
  if (order$loyal) net <- net * 0.95
  if (order$quantity > 5L) net <- net - 1
  tax <- net * rates[[order$region]]
  list(id = order$id, net = net, tax = tax, gross = net + tax)
}
price_orders <- function(n) {
  regions <- c("north", "south", "east", "west")
  rates <- c(north = 0.2, south = 0.1, east = 0.15, west = 0.05)
  total <- 0
  for (i in seq_len(n)) {
    order <- list(id = i, quantity = i %% 7L + 1L, unit = 10 + (i %% 13L) * 0.5, loyal = i %% 3L == 0L, region = regions[[i %% 4L + 1L]])
    invoice <- price_line(order, rates)
    total <- total + invoice$gross
  }
  total
}
measure("orders", function() price_orders(200000L))
