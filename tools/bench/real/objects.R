# Object systems: S3 constructors, validators and methods with UseMethod and NextMethod; S4 classes
# with validity, generics and show; R6 classes with inheritance and active bindings; environments as
# mutable caches. Production code mixes all of them.
new_money <- function(amount, currency = "EUR") {
  stopifnot(is.numeric(amount), is.character(currency))
  structure(list(amount = amount, currency = currency), class = c("money", "value"))
}
format.money <- function(x, ...) paste(formatC(x$amount, format = "f", digits = 2), x$currency)
format.value <- function(x, ...) "<value>"
print.money <- function(x, ...) invisible(format(x, ...))
amount <- function(x, ...) UseMethod("amount")
amount.default <- function(x, ...) stop("no amount")
amount.value <- function(x, ...) x$amount
amount.money <- function(x, rate = 1, ...) NextMethod() * rate
Ops.money <- function(e1, e2) {
  value <- get(.Generic)(e1$amount, if (inherits(e2, "money")) e2$amount else e2)
  if (.Generic %in% c("+", "-", "*", "/")) new_money(value, e1$currency) else value
}

setClass("Instrument", representation(id = "character", notional = "numeric", "VIRTUAL"))
setClass("Bond", contains = "Instrument", representation(coupon = "numeric", maturity = "numeric"),
         validity = function(object) if (object@coupon < 0) "negative coupon" else TRUE)
setClass("Swap", contains = "Instrument", representation(fixed = "numeric", floating = "character"))
invisible(setGeneric("price", function(instrument, rate, ...) standardGeneric("price")))
setMethod("price", "Bond", function(instrument, rate, ...) {
  times <- seq_len(instrument@maturity)
  sum(instrument@coupon * instrument@notional / (1 + rate)^times) + instrument@notional / (1 + rate)^instrument@maturity
})
setMethod("price", "Swap", function(instrument, rate, ...) instrument@notional * (instrument@fixed - rate))
invisible(setGeneric("describe", function(x) standardGeneric("describe")))
setMethod("describe", "Instrument", function(x) paste(class(x), x@id))

Account <- R6::R6Class("Account",
  public = list(
    owner = NULL, history = NULL,
    initialize = function(owner, balance = 0) {
      self$owner <- owner
      private$balance_ <- balance
      self$history <- list()
    },
    deposit = function(x) {
      private$balance_ <- private$balance_ + x
      self$history[[length(self$history) + 1L]] <- x
      invisible(self)
    },
    withdraw = function(x) {
      if (x > private$balance_) stop("insufficient funds")
      private$balance_ <- private$balance_ - x
      invisible(self)
    }
  ),
  private = list(balance_ = 0),
  active = list(balance = function(value) {
    if (missing(value)) private$balance_ else stop("read-only")
  })
)
Savings <- R6::R6Class("Savings", inherit = Account,
  public = list(
    rate = 0.01,
    accrue = function() {
      self$deposit(self$balance * self$rate)
      invisible(self)
    },
    withdraw = function(x) {
      if (x > 1000) stop("limit")
      super$withdraw(x)
    }
  )
)

cache <- new.env(parent = emptyenv())
cached_price <- function(instrument, rate) {
  key <- paste(instrument@id, rate)
  if (!is.null(cache[[key]])) return(cache[[key]])
  value <- price(instrument, rate)
  assign(key, value, envir = cache)
  value
}

objects <- function(n) {
  rm(list = ls(cache), envir = cache)
  total <- new_money(0)
  amounts <- numeric(n)
  for (i in seq_len(n)) {
    m <- new_money(i / 10, "EUR")
    total <- total + m * 2
    amounts[[i]] <- amount(m, rate = 1.1)
  }
  instruments <- lapply(seq_len(n %/% 4L), function(i) {
    if (i %% 2L == 0L) new("Bond", id = paste0("B", i), notional = 100, coupon = 0.02 + i %% 5 / 100, maturity = i %% 10 + 1)
    else new("Swap", id = paste0("S", i), notional = 1e6, fixed = 0.03, floating = "EURIBOR")
  })
  prices <- vapply(instruments, function(x) cached_price(x, 0.01 + (x@notional %% 3) / 100), numeric(1))
  again <- vapply(instruments, function(x) cached_price(x, 0.01 + (x@notional %% 3) / 100), numeric(1))
  labels <- vapply(instruments, describe, character(1))
  accounts <- lapply(seq_len(n %/% 20L), function(i) Savings$new(paste0("owner", i), balance = i))
  for (round in 1:5) {
    for (account in accounts) {
      account$deposit(round)$accrue()
      tryCatch(account$withdraw(round * 300), error = function(e) NULL)
    }
  }
  balances <- vapply(accounts, function(a) a$balance, numeric(1))
  list(
    total = format(total), amounts = sum(amounts), prices = sum(prices), same = identical(prices, again),
    labels = table(substr(labels, 1, 4)), balances = round(sum(balances), 6),
    history = sum(lengths(lapply(accounts, function(a) a$history))), cached = length(ls(cache))
  )
}

measure("objects", function() objects(8000L))
