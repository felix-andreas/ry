#@ weight 3
#@ param COUPON float 0.01 0.08 3
#@ param YEARS int 2 30
#@ param FREQ choice 1|2|4
# Fixed-rate bond analytics for instrument @ID@.
bond_cashflows_@ID@ <- function(face = 100, coupon = @COUPON@, years = @YEARS@L, freq = @FREQ@L) {
  periods <- years * freq
  times <- seq_len(periods) / freq
  flows <- rep(face * coupon / freq, periods)
  flows[[periods]] <- flows[[periods]] + face
  data.frame(time = times, flow = flows)
}

bond_price_@ID@ <- function(yield, flows = bond_cashflows_@ID@(), freq = @FREQ@L) {
  periods <- flows$time * freq
  sum(flows$flow / (1 + yield / freq)^periods)
}

bond_duration_@ID@ <- function(yield, flows = bond_cashflows_@ID@(), freq = @FREQ@L) {
  periods <- flows$time * freq
  present <- flows$flow / (1 + yield / freq)^periods
  sum(flows$time * present) / sum(present)
}

bond_convexity_@ID@ <- function(yield, flows = bond_cashflows_@ID@(), freq = @FREQ@L) {
  periods <- flows$time * freq
  present <- flows$flow / (1 + yield / freq)^periods
  sum(present * periods * (periods + 1)) / (sum(present) * (1 + yield / freq)^2 * freq^2)
}

bond_yield_@ID@ <- function(price, flows = bond_cashflows_@ID@(), freq = @FREQ@L) {
  stats::uniroot(
    function(y) bond_price_@ID@(y, flows, freq) - price,
    interval = c(1e-6, 1), tol = 1e-10
  )$root
}
#@ test
test_that("bond @ID@ prices and inverts", {
  flows <- bond_cashflows_@ID@()
  expect_equal(nrow(flows), @YEARS@ * @FREQ@)
  at_par <- bond_price_@ID@(@COUPON@, flows)
  expect_equal(at_par, 100, tolerance = 1e-8)
  expect_lt(bond_price_@ID@(@COUPON@ + 0.01, flows), at_par)
  expect_equal(bond_yield_@ID@(bond_price_@ID@(0.05, flows), flows), 0.05, tolerance = 1e-6)
  expect_gt(bond_duration_@ID@(0.05, flows), 0)
  expect_gt(bond_convexity_@ID@(0.05, flows), 0)
})
