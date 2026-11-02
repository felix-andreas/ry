#@ weight 2
#@ ns importFrom(zeallot,"%<-%")
#@ param DAYS int 5 120
# Date arithmetic with lubridate, and destructuring with zeallot.
business_days_@ID@ <- function(start: Date, days: integer = @DAYS@L) -> Date {
  candidates <- start + lubridate::days(seq_len(days * 2L))
  weekdays_only <- candidates[!lubridate::wday(candidates) %in% c(1L, 7L)]
  business: Date <- utils::head(weekdays_only, days)
  business
}

span_summary_@ID@ <- function(dates: Date) -> list{first: Date, last: Date, months: double[], quarters: double[]} {
  bounds: Date <- range(dates)
  c(first, last) %<-% bounds
  list(
    first = first,
    last = last,
    months = lubridate::interval(first, last) %/% months(1),
    quarters = unique(lubridate::quarter(dates, with_year = TRUE))
  )
}

month_ends_@ID@ <- function(year: double) -> Date {
  starts <- lubridate::make_date(year, 1:12, 1L)
  lubridate::ceiling_date(starts, "month") - lubridate::days(1)
}
#@ test
test_that("calendar @ID@ skips weekends", {
  start <- as.Date("2024-01-01")
  days <- business_days_@ID@(start)
  expect_length(days, @DAYS@)
  expect_false(any(lubridate::wday(days) %in% c(1L, 7L)))
  summary <- span_summary_@ID@(days)
  expect_true(summary$last >= summary$first)
  expect_length(month_ends_@ID@(2024), 12L)
  expect_equal(month_ends_@ID@(2024)[[2]], as.Date("2024-02-29"))
})
