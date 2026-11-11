# A hot path from the finance code base: maximum drawdown and EWMA volatility, scalar state
# carried through one pass over a return series.
risk_report <- function(returns: List<F64>, lambda: F64 = 0.94) -> F64 {
  wealth <- 1
  peak <- 1
  worst <- 0
  first <- returns[[1]]
  variance <- first * first
  for (r in returns) {
    wealth <- wealth * (1 + r)
    if (wealth > peak) peak <- wealth
    drawdown <- (peak - wealth) / peak
    if (drawdown > worst) worst <- drawdown
    variance <- lambda * variance + (1 - lambda) * r * r
  }
  worst + Num.sqrt(variance * 252)
}
returns <- List.from_r(sin(seq_len(2000000L) * 0.37) * 0.01 + 0.0002)
measure("drawdown", function() risk_report(returns))
