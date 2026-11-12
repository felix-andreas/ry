# blocked: scaled_total
# boundary: scaled_total
# The native local function `scale` reads a variable of the boundary function around it. The
# header names the top-level function a finding lies in.
scaled_total <- function(values: double[]) -> double {
  factor <- 2f64
  total <- List.sum(List.from_r(values))
  scale <- function(x: F64) -> F64 x * factor
  scale(total)
}
cat(scaled_total(c(1, 2)), "\n")
