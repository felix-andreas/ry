# oracle: requires data.table
# What a function called from a grouped `j` observes of its caller once data.table's per-group
# evaluation runs as compiled code, and the conditions a late group raises.
library(data.table)
DT <- data.table(g = rep(1:12, each = 3L), v = as.numeric(1:36))
calls <- function(x, y) {
  list(
    call = deparse(sys.call()),
    matched = deparse(match.call()),
    substituted = deparse(substitute(x)),
    missing = missing(y),
    parent_has_v = exists("v", envir = parent.frame(), inherits = FALSE),
    own = environmentName(parent.env(environment()))
  )
}
str(DT[, .(info = list(calls(v * 2))), by = g]$info[[12L]])
str(DT[, .(info = list(calls(v, y = .N))), by = g]$info[[1L]])
# An error in the tenth group, with its message and call as R reports them.
tryCatch(
  DT[, if (.GRP == 10L) stop("group ", .GRP, " failed") else sum(v), by = g],
  error = function(e) cat("error:", conditionMessage(e), "| call:", deparse(conditionCall(e)), "\n")
)
checked <- function(x) {
  if (sum(x) > 90) warning("sum ", sum(x), " is large")
  sum(x)
}
withCallingHandlers(
  DT[, checked(v), by = g][, V1],
  warning = function(w) {
    cat("warning:", conditionMessage(w), "| call:", deparse(conditionCall(w)), "\n")
    invokeRestart("muffleWarning")
  }
)
tryCatch(
  DT[, v[[.GRP * 10L]], by = g],
  error = function(e) cat("error:", conditionMessage(e), "\n")
)
# A function that a later group redefines in the global environment.
scale_of <- function(x) sum(x)
DT[, { if (.GRP == 6L) scale_of <<- function(x) -1; scale_of(v) }, by = g]$V1
