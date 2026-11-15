# formatC()'s prologue reads its formals in one pass and never forces a formal left to its
# default. Arguments with side effects show that the given ones are still forced in the order
# the R body forces them, and explicit defaults, named and reordered arguments, arguments passed
# on from a caller, and options read by the defaults give the body's answers.
show <- function(x) print(x)
caught <- function(expr) withCallingHandlers(
  tryCatch(expr, error = function(e) cat("error:", conditionMessage(e), "\n")),
  warning = function(w) {
    cat("warning:", conditionMessage(w), "\n")
    invokeRestart("muffleWarning")
  }
)
loud <- function(name, value) {
  cat(name, "")
  value
}
show(formatC(5L, width = 6L, flag = "0"))
show(formatC(flag = "0", width = 6L, x = 5L))
show(formatC(5L, NULL, 6L, NULL, "0"))
show(formatC(5L, digits = NULL, width = 6L, format = NULL, flag = "0", mode = NULL,
  big.mark = "", big.interval = 3L, small.mark = "", small.interval = 5L,
  decimal.mark = ".", preserve.width = "individual", zero.print = NULL,
  replace.zero = TRUE, drop0trailing = FALSE))
show(formatC(loud("x", 5L), width = loud("width", 6L), flag = loud("flag", "0"),
  digits = loud("digits", NULL), format = loud("format", "d"), mode = loud("mode", NULL),
  big.mark = loud("big.mark", ""), small.mark = loud("small.mark", ""),
  decimal.mark = loud("decimal.mark", "."), zero.print = loud("zero.print", NULL),
  drop0trailing = loud("drop0trailing", FALSE)))
show(formatC(loud("x", 1234567L), big.mark = loud("big.mark", ","),
  big.interval = loud("big.interval", 3L), preserve.width = loud("preserve.width", "none"),
  small.mark = loud("small.mark", ""), decimal.mark = loud("decimal.mark", "."),
  zero.print = loud("zero.print", NULL), drop0trailing = loud("drop0trailing", FALSE),
  width = loud("width", 12L)))
show(formatC(1234567L, big.mark = ",", big.interval = 2L))
show(formatC(1234567L, big.mark = ",", preserve.width = "none", width = 12L))
# Integers at the edges of the range, through `%d` and every flag that pads.
edges <- c(0L, -1L, 9L, 10L, -10L, 99999L, -100000L, .Machine$integer.max, -.Machine$integer.max)
for (fl in c("", "0", "-", "+", " ", "0+", "0 ", "-+")) {
  show(formatC(edges, width = 12L, flag = fl))
  show(formatC(as.numeric(edges), width = 12L, format = "d", flag = fl))
}
show(formatC(c(-0.5, 0.5, 1.5, 2.5, -2.5, 1e9 + 0.5), format = "d"))
show(formatC(c(-0.5, 0.5, 1.5, 2.5, -2.5), width = 5L, format = "d", flag = "0"))
show(formatC(1:3, width = 0L))
show(formatC(1:3, width = -4L))
show(formatC(1:3, width = 1e6 - 999990L, flag = "0"))
# Arguments passed on from a caller, given and missing.
pass <- function(x, w, f, m) formatC(x, width = w, flag = f, format = m)
show(pass(5L, 4L, "0", "d"))
caught(pass(5L, 4L, "0"))
caught(pass(5L, , "0", "d"))
padded <- function(i) formatC(i, width = 6L, flag = "0")
show(vapply(c(1L, 22L, 333L, 123456L, 1234567L), padded, ""))
for (i in 1:3) show(paste0("ACC", formatC(i * 7L, width = 6L, flag = "0")))
# `decimal.mark` defaults to the `OutDec` option, read when the call runs.
op <- options(OutDec = ",")
show(formatC(c(1.5, 2), format = "f", digits = 1L))
show(formatC(1234567L, big.mark = "."))
options(op)
show(formatC(c(1.5, 2), format = "f", digits = 1L))
local({
  options(OutDec = ".")
  show(formatC(3L, width = 3L, flag = "0"))
})
# sprintf's integer conversions write their digits without the general formatter.
show(sprintf("%d|%5d|%-5d|%05d|%+d|% d", edges, edges, edges, edges, edges, edges))
show(sprintf("%.3d|%8.4d|%x|%o", edges, edges, edges, edges))
show(sprintf("ACC%06d", c(1L, 42L, 999999L)))
