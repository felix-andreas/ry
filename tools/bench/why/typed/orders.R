# Business logic over records: price each order line with a loyalty discount and a regional tax,
# then total the invoices.
alias Region = :North | :South | :East | :West
alias Order = Record{id: integer, quantity: integer, unit: double, loyal: logical, region: Region}
alias Invoice = Record{id: integer, net: double, tax: double, gross: double}

tax_rate <- function(region: Region) -> double {
  match (region) {
    :North => 0.2
    :South => 0.1
    :East => 0.15
    :West => 0.05
  }
}
price_line <- function(order: Order) -> Invoice {
  net <- order$quantity * order$unit
  if (order$loyal) net <- net * 0.95
  if (order$quantity > 5L) net <- net - 1
  tax <- net * tax_rate(order$region)
  (id = order$id, net = net, tax = tax, gross = net + tax)
}
region_of <- function(i: integer) -> Region {
  k <- i %% 4L
  if (k == 0L) :North else if (k == 1L) :South else if (k == 2L) :East else :West
}
price_orders <- function(n: integer) -> double {
  total <- 0
  for (i in seq_len(n)) {
    order <- (id = i, quantity = i %% 7L + 1L, unit = 10 + (i %% 13L) * 0.5, loyal = i %% 3L == 0L, region = region_of(i))
    invoice <- price_line(order)
    total <- total + invoice$gross
  }
  total
}
measure("orders", function() price_orders(200000L))
