# A hot path from the finance code base: maximum drawdown and EWMA volatility, scalar state
# carried through one pass over a return series.
risk_report <- function(returns: double[], lambda: double = 0.94) -> double[] {
  wealth <- 1
  peak <- 1
  worst <- 0
  variance <- returns[1] * returns[1]
  for (i in seq_along(returns)) {
    r <- returns[i]
    wealth <- wealth * (1 + r)
    if (wealth > peak) peak <- wealth
    drawdown <- (peak - wealth) / peak
    if (drawdown > worst) worst <- drawdown
    variance <- lambda * variance + (1 - lambda) * r * r
  }
  c(max_drawdown = worst, volatility = sqrt(variance * 252))
}
returns <- sin(seq_len(2000000L) * 0.37) * 0.01 + 0.0002
measure("drawdown", function() sum(risk_report(returns)))
