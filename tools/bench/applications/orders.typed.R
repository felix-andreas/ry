# Business logic over records: price each order line with a loyalty discount and a regional tax,
# then total the invoices.
alias Region = :North | :South | :East | :West
alias Order = Record{id: I64, quantity: I64, unit: F64, loyal: Bool, region: Region}
alias Invoice = Record{id: I64, net: F64, tax: F64, gross: F64}

tax_rate <- function(region: Region) -> F64 {
  match (region) {
    :North => 0.2
    :South => 0.1
    :East => 0.15
    :West => 0.05
  }
}
price_line <- function(order: Order) -> Invoice {
  net <- Num.to_f64(order$quantity) * order$unit
  if (order$loyal) net <- net * 0.95
  if (order$quantity > 5) net <- net - 1
  tax <- net * tax_rate(order$region)
  (id = order$id, net = net, tax = tax, gross = net + tax)
}
region_of <- function(i: I64) -> Region {
  k <- i %% 4
  if (k == 0) :North else if (k == 1) :South else if (k == 2) :East else :West
}
price_orders <- function(n: I64) -> F64 {
  total <- 0
  for (i in List.range(1, n)) {
    order <- (id = i, quantity = i %% 7 + 1, unit = 10 + Num.to_f64(i %% 13) * 0.5, loyal = i %% 3 == 0, region = region_of(i))
    invoice <- price_line(order)
    total <- total + invoice$gross
  }
  total
}
measure("orders", function() price_orders(200000))
