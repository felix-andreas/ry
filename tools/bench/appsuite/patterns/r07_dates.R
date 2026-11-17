# bench(label, f) times f(i). It doubles the number of calls until one batch takes 0.3 s of CPU
# time, then prints the label and the nanoseconds per call of the last batch. With the environment
# variable UNDER_HANDLER=1, the calls run inside withCallingHandlers(), as the code of a test does.
under_handler <- nzchar(Sys.getenv("UNDER_HANDLER"))
run <- function(n, f) for (i in seq_len(n)) f(i)
bench <- function(label, f) {
  n <- 64
  repeat {
    start <- sum(proc.time()[1:2])
    if (under_handler) withCallingHandlers(run(n, f), warning = function(w) NULL) else run(n, f)
    seconds <- sum(proc.time()[1:2]) - start
    if (seconds >= 0.3) break
    n <- n * 2
  }
  cat(sprintf("%s\t%.0f\n", label, 1e9 * seconds / n))
}

# Pattern: date arithmetic on single dates and on short date vectors.
day <- as.Date("2024-01-31")
days <- day + 0:19
holidays <- as.Date("2024-01-01") + c(0, 90, 120, 359)
stamp <- as.POSIXct("2024-01-31 12:00:00", tz = "UTC")

bench("dates: as.Date() of a text", function(i) as.Date("2024-01-31"))
bench("dates: as.Date() of 20 texts", function(i) as.Date(c("2024-01-31", "2024-02-29")[rep(1:2, 10)]))
bench("dates: format(day, '%Y-%m-%d')", function(i) format(day, "%Y-%m-%d"))
bench("dates: as.character() of 20 dates", function(i) as.character(days))
bench("dates: as.POSIXlt(day)$year", function(i) as.POSIXlt(day)$year)
bench("dates: as.POSIXlt() of 20 dates, $mon", function(i) as.POSIXlt(days)$mon)
bench("dates: as.POSIXlt() of a POSIXct", function(i) as.POSIXlt(stamp))
bench("dates: weekdays() of 20 dates", function(i) weekdays(days))
bench("dates: day + 1 and difference in days", function(i) as.numeric((day + i) - day))
bench("dates: day %in% holidays", function(i) day %in% holidays)
bench("dates: seq() of dates by day", function(i) seq(day, by = "day", length.out = 20))
bench("dates: seq() of dates by month", function(i) seq(day, by = "month", length.out = 12))
bench("dates: max() and range() of 20 dates", function(i) range(days))
bench("dates: is.na(day) and day > other", function(i) !is.na(day) && day > holidays[1])
bench("dates: as.numeric(format(day, '%Y'))", function(i) as.numeric(format(day, "%Y")))
bench("dates: Sys.time() and Sys.Date()", function(i) Sys.time() > Sys.Date())
bench("dates: lubridate::year() and month()", function(i) lubridate::year(day) * 100 + lubridate::month(day))
bench("dates: lubridate::ymd() of a text", function(i) lubridate::ymd("2024-01-31"))
bench("dates: c() of two dates", function(i) c(day, day + 1))
