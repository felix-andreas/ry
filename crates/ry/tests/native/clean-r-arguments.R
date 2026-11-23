# R evaluates a literal with no ry type and a variable from outside the function itself, when
# either is an argument of an R call, and reads the variable when the call runs.
alias Sales = DataFrame{region: character, units: double}
#: double
threshold <- 1.5
#: character[]
months <- c("jan", "feb")
large <- function(sales: Sales) -> Sales {
  sales |> DataFrame.filter(Expr.col("units") > threshold)
}
blanked <- function(sales: Sales) -> Sales {
  DataFrame.with_columns(
    sales,
    region = Expr.if_else(Expr.col("units") > 1, Expr.col("region"), Expr.lit(NA_character_)),
    units = Expr.if_else(Expr.col("units") > 1, Expr.col("units"), Expr.lit(NA_real_))
  )
}
labels <- function(label: Str) -> Str {
  Str.from_r(paste(label, months, NA, 2i, sep = "-", collapse = " "))
}
sales <- DataFrame(region = c("north", "south"), units = c(1, 2))
print(large(sales))
threshold <- 0.5
print(large(sales))
print(blanked(sales))
cat(labels("q"), "\n")
cat(labels("r"), "\n")
