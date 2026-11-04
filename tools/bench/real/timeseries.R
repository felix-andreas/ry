# Time series and dates: xts and zoo series built from a price path, merged, lagged, differenced,
# aggregated by period and rolled; lubridate parsing, rounding and month arithmetic; base Date
# sequences and formatting. Market-data and reporting code spends its time here.
suppressPackageStartupMessages({
  library(xts)
  library(lubridate)
})

price_series <- function(n, seed, start = as.POSIXct("2024-01-02 09:00:00", tz = "UTC")) {
  set.seed(seed)
  index <- start + seq(0, by = 60, length.out = n)
  xts(cbind(price = 100 * cumprod(1 + rnorm(n, 0, 0.001)), volume = round(runif(n, 1, 100))), order.by = index)
}

daily_stats <- function(series) {
  returns <- diff(log(series$price))
  hourly <- period.apply(series$price, endpoints(series, "hours"), function(p) c(max(p), min(p)))
  ohlc <- to.period(series$price, "days", OHLC = TRUE)
  rolled <- zoo::rollapply(series$price, 30, mean, align = "right")
  vwap <- sum(series$price * series$volume) / sum(series$volume)
  list(vol = sd(returns, na.rm = TRUE), hours = nrow(hourly), days = nrow(ohlc),
       rolled = mean(rolled, na.rm = TRUE), vwap = as.numeric(vwap))
}

schedule <- function(start, months, day_count = 360) {
  dates <- start %m+% months(seq_len(months))
  fractions <- as.numeric(diff(c(start, dates))) / day_count
  data.frame(date = format(dates, "%Y-%m-%d"), fraction = fractions, weekday = wday(dates, label = FALSE))
}

timeseries <- function(n) {
  stats <- lapply(1:6, function(seed) daily_stats(price_series(n, seed)))
  a <- price_series(n %/% 2, 7)
  b <- price_series(n %/% 2, 8)
  merged <- merge(a$price, lag(b$price, 5), join = "inner")
  filled <- zoo::na.locf(merged, na.rm = FALSE)
  window <- a["2024-01-02 10:00/2024-01-02 12:00"]
  stamps <- sprintf("2024-%02d-%02d %02d:%02d:00", rep(1:12, length.out = n %/% 4), rep(1:28, length.out = n %/% 4),
                    rep(0:23, length.out = n %/% 4), rep(0:59, length.out = n %/% 4))
  parsed <- ymd_hms(stamps, tz = "UTC")
  floored <- floor_date(parsed, "week")
  months_between <- interval(parsed[[1]], parsed) %/% months(1)
  schedules <- lapply(seq_len(40), function(i) schedule(ymd("2024-01-31") + days(i), 24))
  dates <- seq(as.Date("2020-01-01"), by = "day", length.out = n %/% 4)
  labels <- format(dates, "%b %Y")
  list(
    vol = round(sum(vapply(stats, function(s) s$vol, numeric(1))), 10),
    hours = sum(vapply(stats, function(s) s$hours, numeric(1))),
    merged = nrow(filled), window = nrow(window),
    floored = length(unique(floored)), months = sum(months_between, na.rm = TRUE),
    schedules = sum(vapply(schedules, function(s) sum(s$fraction), numeric(1))),
    labels = length(unique(labels))
  )
}

measure("timeseries", function() timeseries(8000L))
