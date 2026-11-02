#@ weight 3
#@ param SYMBOLS int 2 8
#@ param DAYS int 3 30
# Market-data adapters: the live Bloomberg source is injected, tests use a generated one.
#@ typed
alias Prices@ID@ = DataFrame{symbol: character[], date: Date, price: double[]}
#@ untyped
alias Prices@ID@ = data.table{symbol: character[], date: Date, price: double[]}
#@ end

fetch_prices_@ID@ <- function(symbols: character[], start: Date, end: Date, source: (fn(symbols: character[], start: Date, end: Date) -> Prices@ID@) | NULL = NULL) -> Prices@ID@ {
  if (is.null(source)) {
    connection <- Rblpapi::blpConnect()
    source <- function(symbols, start, end) Rblpapi::bdh(symbols, "PX_LAST", start, end, con = connection)
  }
  source(symbols, start, end)
}

synthetic_source_@ID@ <- function(symbols: character[], start: Date, end: Date) -> Prices@ID@ {
  dates <- seq(start, end, by = "day")
#@ typed
  index <- rep(seq_along(symbols), each = length(dates))
  day <- rep(seq_along(dates), times = length(symbols))
  DataFrame(symbol = symbols[index], date = dates[day], price = 100 + index + day * 0.1)
#@ untyped
  out <- lapply(seq_along(symbols), function(i) {
    data.table::data.table(symbol = symbols[[i]], date = dates, price = 100 + i + seq_along(dates) * 0.1)
  })
  data.table::rbindlist(out)
#@ end
}

#@ typed
latest_prices_@ID@ <- function(prices: Prices@ID@) -> DataFrame{symbol: character[], price: double[], observations: integer[]} {
  GroupBy.agg(DataFrame.group_by(DataFrame.sort(prices, "date"), "symbol"), price = Expr.last(col("price")), observations = Expr.len())
}

to_wide_@ID@ <- function(prices: Prices@ID@) -> DataFrame {
  shape: character <- "symbol"
  DataFrame.pivot(prices, on = shape, index = "date", values = "price")
}

read_reference_@ID@ <- function(path: character) -> DataFrame {
  DataFrame.read_csv(path)
}
#@ untyped
latest_prices_@ID@ <- function(prices: Prices@ID@) -> data.table{symbol: character[], price: double[], observations: integer[]} {
  prices[order(date), .(price = data.table::last(price), observations = .N), by = symbol]
}

to_wide_@ID@ <- function(prices: Prices@ID@) -> data.table {
  shape: formula <- date ~ symbol
  data.table::dcast(prices, shape, value.var = "price")
}

read_reference_@ID@ <- function(path: character) -> data.table {
  data.table::fread(path)
}
#@ end
#@ test
test_that("market data @ID@ shapes prices", {
  symbols <- paste0("SYM", seq_len(@SYMBOLS@))
  start <- as.Date("2024-01-01")
  prices <- fetch_prices_@ID@(symbols, start, start + @DAYS@ - 1L, source = synthetic_source_@ID@)
  expect_equal(nrow(prices), @SYMBOLS@ * @DAYS@)
  latest <- latest_prices_@ID@(prices)
  expect_equal(latest$observations, rep(@DAYS@, @SYMBOLS@))
  wide <- to_wide_@ID@(prices)
  expect_equal(ncol(wide), @SYMBOLS@ + 1L)
})
