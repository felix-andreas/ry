# List building and modification: records as named lists, grown, updated with `$<-` and `[[<-`,
# merged with modifyList, grouped with split, folded with Reduce, and turned into a data frame with
# do.call(rbind, ...). This is how most R code that is not a vectorized kernel shapes its data.
make_trade <- function(id, book, quantity, price, side = "buy", tags = character()) {
  list(id = id, book = book, quantity = quantity, price = price, side = side, tags = tags,
       notional = quantity * price)
}

apply_fee <- function(trade, rate = 0.001, minimum = 1) {
  trade$fee <- max(trade$notional * rate, minimum)
  trade[["net"]] <- trade$notional + if (trade$side == "buy") trade$fee else -trade$fee
  trade
}

summarise_book <- function(trades) {
  notional <- vapply(trades, function(t) t$notional, numeric(1))
  fees <- vapply(trades, `[[`, numeric(1), "fee")
  list(count = length(trades), notional = sum(notional), fees = sum(fees),
       largest = trades[[which.max(notional)]]$id)
}

records <- function(n) {
  books <- c("rates", "credit", "equity", "fx", "commodities")
  trades <- vector("list", n)
  for (i in seq_len(n)) {
    trade <- make_trade(i, books[[i %% 5L + 1L]], quantity = (i %% 17) + 1, price = 100 + (i %% 31) / 4,
                        side = if (i %% 3L == 0L) "sell" else "buy")
    if (i %% 4L == 0L) trade$tags <- c(trade$tags, "large")
    trades[[i]] <- apply_fee(trade)
  }
  by_book <- split(trades, vapply(trades, function(t) t$book, character(1)))
  summaries <- lapply(by_book, summarise_book)
  defaults <- list(currency = "EUR", desk = "london", limits = list(var = 1e6, notional = 5e7))
  configs <- lapply(names(summaries), function(book) {
    utils::modifyList(defaults, list(book = book, limits = list(var = summaries[[book]]$notional / 100)))
  })
  rows <- lapply(trades[seq_len(min(n, 400L))], function(t) {
    data.frame(id = t$id, book = t$book, net = t$net, stringsAsFactors = FALSE)
  })
  frame <- do.call(rbind, rows)
  totals <- Reduce(function(acc, t) {
    acc[[t$book]] <- (acc[[t$book]] %||% 0) + t$net
    acc
  }, trades, list())
  nested <- Map(function(s, cfg) c(s, cfg["limits"]), summaries, configs)
  list(
    summaries = summaries, totals = unlist(totals)[order(names(totals))], rows = nrow(frame),
    net = sum(frame$net), nested = length(unlist(nested))
  )
}

`%||%` <- function(a, b) if (is.null(a)) b else a

measure("records", function() records(25000L))
