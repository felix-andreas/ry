# Native code reads a global that holds a ry value where it runs, so it sees a rebinding.
alias Sales = DataFrame{region: character, units: double}
rate: F64 <- 0.2
count: I64 <- 3
label: Str <- "total"
verbose: Bool <- TRUE
weights: List<F64> <- [1.5, 2.5]
#: Expr
positive <- Expr.col("units") > 0
#: Sales
sales <- DataFrame(region = c("north", "south"), units = c(-1, 2))
taxed <- function(amount: F64) -> F64 amount * (1 + rate) * List.sum(weights)
scaled <- function(n: I64) -> I64 n * count
named <- function(suffix: Str) -> Str if (verbose) Str.concat(label, suffix) else suffix
kept <- function(least: F64) -> Sales {
  above <- DataFrame.filter(sales, positive)
  DataFrame.filter(above, Expr.col("units") > Num.to_r(least * rate))
}
cat(taxed(100), Num.to_r(scaled(2)), named("!"), "\n")
print(kept(1))
rate <- 0.5
count <- 4
cat(taxed(100), Num.to_r(scaled(2)), "\n")
