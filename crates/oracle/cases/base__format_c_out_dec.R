# formatC()'s decimal.mark defaults to getOption("OutDec"), so the option reaches every call that
# leaves the mark out, with or without a big mark, while an explicit mark overrides it.
show <- function(x) print(x)
caught <- function(expr) withCallingHandlers(
  tryCatch(expr, error = function(e) cat("error:", conditionMessage(e), "\n")),
  warning = function(w) {
    cat("warning:", conditionMessage(w), "\n")
    invokeRestart("muffleWarning")
  }
)
values <- c(1234567.891, -2.5, 0, NA)
cases <- function() {
  show(formatC(values, format = "f", digits = 2L))
  show(formatC(values, format = "f", digits = 2L, big.mark = " "))
  show(formatC(values, format = "e", digits = 3L))
  show(formatC(values))
  show(formatC(c(12L, -3L), width = 6L, flag = "0"))
  show(formatC(values, format = "f", digits = 1L, decimal.mark = "."))
  show(formatC(values, format = "f", digits = 1L, decimal.mark = ","))
  wrapper <- function(x, mark = getOption("OutDec")) formatC(x, format = "f", decimal.mark = mark)
  show(wrapper(3.25))
  show(vapply(1:3, function(i) formatC(i * 1.5, format = "f", digits = 1L), ""))
}
cases()
old <- options(OutDec = ",")
cases()
caught(formatC(1234.5, format = "f", big.mark = ","))
options(OutDec = ".")
cases()
options(old)
show(getOption("OutDec"))
