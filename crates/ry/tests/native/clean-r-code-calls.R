# An argument of an R call made only of R code (literals, variables from outside the function,
# and calls of R functions on them) is a promise R evaluates when the call asks for it.
alias Days = DataFrame{day: Date, v: double}
#: character
start <- "2026-01-01"
since <- function(rows: Days) -> Days {
  rows |> DataFrame.filter(Expr.col("day") >= as.Date(start))
}
fixed <- function(rows: Days) -> Days {
  rows |> DataFrame.filter(Expr.col("day") < base::as.Date("2026-01-02"))
}
shout <- function(word: Str) -> Str {
  Str.from_r(paste(word, toupper(trimws(start)), nchar(rep(start, 2L)), sep = "|", collapse = ","))
}
rows <- DataFrame(day = as.Date(c("2025-12-31", "2026-01-02")), v = c(1, 2))
print(since(rows))
start <- "2025-01-01"
print(since(rows))
print(fixed(rows))
cat(shout("a"), "\n")
