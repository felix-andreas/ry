#@ weight 3
#@ param SYMBOLS int 2 8
#@ param DAYS int 3 30
# Market-data adapters: the live Bloomberg source is injected, tests use a generated one.
#: @alias Prices@ID@ {data.table{symbol: character[], date: Date, price: double[]}}

#: fn(symbols: character[], start: Date, end: Date, [source]: (fn(symbols: character[], start: Date, end: Date) -> Prices@ID@) | NULL) -> Prices@ID@
fetch_prices_@ID@ <- function(symbols, start, end, source = NULL) {
  if (is.null(source)) {
    connection <- Rblpapi::blpConnect()
    source <- function(symbols, start, end) Rblpapi::bdh(symbols, "PX_LAST", start, end, con = connection)
  }
  source(symbols, start, end)
}

#: fn(symbols: character[], start: Date, end: Date) -> Prices@ID@
synthetic_source_@ID@ <- function(symbols, start, end) {
  dates <- seq(start, end, by = "day")
  out <- lapply(seq_along(symbols), function(i) {
    data.table::data.table(symbol = symbols[[i]], date = dates, price = 100 + i + seq_along(dates) * 0.1)
  })
  data.table::rbindlist(out)
}

#: fn(prices: Prices@ID@) -> data.table{symbol: character[], price: double[], observations: integer[]}
latest_prices_@ID@ <- function(prices) {
  prices[order(date), .(price = data.table::last(price), observations = .N), by = symbol]
}

#: fn(prices: Prices@ID@) -> data.table
to_wide_@ID@ <- function(prices) {
  #: formula
  shape <- date ~ symbol
  data.table::dcast(prices, shape, value.var = "price")
}

#: fn(path: character) -> data.table
read_reference_@ID@ <- function(path) {
  data.table::fread(path)
}
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
