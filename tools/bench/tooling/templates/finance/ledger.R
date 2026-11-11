#@ weight 2
#@ param TRADES int 200 600
# Trade fills folded into a position, its cost and the realised profit. The typed program models a
# fill as a tag and the book as a record; the untyped one uses lists with a `kind` field.
alias Fill@ID@ = :Buy(quantity: double, price: double) | :Sell(quantity: double, price: double) | :Fee(amount: double)

fills_@ID@ <- function(count: integer = @TRADES@L, seed: integer = 11L) -> list[Fill@ID@] {
  set.seed(seed)
  sides: integer[] <- sample(c(1L, 2L, 3L), count, replace = TRUE, prob = c(0.45, 0.45, 0.1))
  quantity: double[] <- round(stats::runif(count, 1, 100))
  price: double[] <- round(100 + cumsum(stats::rnorm(count)), 2)
  out: list[Fill@ID@] <- vector("list", count)
  for (i in seq_len(count)) {
#@ typed
    out[[i]] <- if (sides[[i]] == 1L) :Buy(quantity = quantity[[i]], price = price[[i]]) else if (sides[[i]] == 2L) :Sell(quantity = quantity[[i]], price = price[[i]]) else :Fee(amount = quantity[[i]] / 100)
#@ untyped
    out[[i]] <- if (sides[[i]] == 1L) list(kind = "buy", quantity = quantity[[i]], price = price[[i]]) else if (sides[[i]] == 2L) list(kind = "sell", quantity = quantity[[i]], price = price[[i]]) else list(kind = "fee", amount = quantity[[i]] / 100)
#@ end
  }
  out
}

book_@ID@ <- function(fills: list[Fill@ID@]) -> Record{position: double, cost: double, realised: double} {
  position <- 0
  cost <- 0
  realised <- 0
  for (fill in fills) {
#@ typed
    step <- match (fill) {
      :Buy(quantity = q, price = p) => (q, p)
      :Sell(quantity = q, price = p) => (-q, p)
      :Fee(amount = a) => (0, a)
    }
#@ untyped
    step <- switch(fill$kind,
      buy = c(fill$quantity, fill$price),
      sell = c(-fill$quantity, fill$price),
      fee = c(0, fill$amount)
    )
#@ end
    delta <- step[[1L]]
    if (delta == 0) {
      realised <- realised - step[[2L]]
    } else if (delta > 0) {
      cost <- cost + delta * step[[2L]]
      position <- position + delta
    } else {
      average <- if (position > 0) cost / position else step[[2L]]
      realised <- realised - delta * (step[[2L]] - average)
      cost <- cost + delta * average
      position <- position + delta
    }
  }
#@ typed
  (position = position, cost = cost, realised = realised)
#@ untyped
  list(position = position, cost = cost, realised = realised)
#@ end
}

ledger_total_@ID@ <- function(books: integer, count: integer = @TRADES@L) -> double {
  total <- 0
  for (seed in seq_len(books)) {
    total <- total + book_@ID@(fills_@ID@(count, seed))$realised
  }
  total
}

#@ test
test_that("book @ID@ folds fills into a position and profit", {
#@ typed
  fills <- list(:Buy(quantity = 10, price = 5), :Sell(quantity = 4, price = 6), :Fee(amount = 1))
#@ untyped
  fills <- list(list(kind = "buy", quantity = 10, price = 5), list(kind = "sell", quantity = 4, price = 6), list(kind = "fee", amount = 1))
#@ end
  book <- book_@ID@(fills)
  expect_equal(book$position, 6)
  expect_equal(book$cost, 30)
  expect_equal(book$realised, 3)
  expect_true(is.finite(ledger_total_@ID@(3L)))
})
