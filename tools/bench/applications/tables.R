# Tables: data.table grouped aggregation, `:=` updates, keyed joins, ordering and rbindlist, run
# many times on small and medium tables as per-entity reporting code does, plus the same steps on
# base data frames (merge, aggregate, order, split).
library(data.table)

make_positions <- function(n, seed) {
  set.seed(seed)
  data.table(
    id = seq_len(n),
    book = sample(c("rates", "credit", "equity", "fx"), n, replace = TRUE),
    desk = sample(sprintf("desk%02d", 1:12), n, replace = TRUE),
    quantity = round(runif(n, 1, 1000)),
    price = round(rlnorm(n, 4, 0.3), 2)
  )
}

desk_report <- function(dt, threshold = 50000) {
  dt[, notional := quantity * price]
  by_desk <- dt[, .(notional = sum(notional), count = .N, largest = max(notional)), by = .(book, desk)]
  by_desk[, share := notional / sum(notional), by = book]
  setorder(by_desk, book, -notional)
  by_desk[notional > threshold]
}

tables <- function(n) {
  limits <- data.table(desk = sprintf("desk%02d", 1:12), limit = seq(1e5, 1.2e6, length.out = 12), key = "desk")
  reports <- vector("list", 60)
  for (day in seq_along(reports)) {
    dt <- make_positions(n, day)
    report <- desk_report(dt)
    setkey(report, desk)
    joined <- limits[report]
    joined[, breach := notional > limit]
    reports[[day]] <- joined[, .(day = day, breaches = sum(breach), notional = sum(notional)), by = book]
  }
  all <- rbindlist(reports)
  summary <- all[, .(breaches = sum(breaches), notional = mean(notional)), keyby = book]
  frame <- as.data.frame(make_positions(n, 99))
  aggregated <- aggregate(price ~ book, data = frame, FUN = mean)
  merged <- merge(frame, as.data.frame(limits), by = "desk")
  ordered <- merged[order(merged$book, -merged$price), ]
  per_book <- vapply(split(ordered$price, ordered$book), function(p) round(sum(p), 2), numeric(1))
  list(summary = summary, aggregated = aggregated, rows = nrow(ordered), per_book = per_book)
}

measure("tables", function() tables(5000L))
