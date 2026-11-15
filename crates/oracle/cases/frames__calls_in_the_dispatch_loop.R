deep <- function(n) if (n == 0L) 0L else 1L + deep(n - 1L)
for (i in 1:3) cat(deep(2000L), "")
cat("\n")

returner <- function(x) { x; "not reached" }
caller <- function() { returner(return("from the promise")); "not reached either" }
for (i in 1:3) cat(caller(), "")
cat("\n")

defaulted <- function(a = return("from the default")) { a; "not reached" }
for (i in 1:3) cat(defaulted(), "")
cat("\n")

trace <- character()
inner_call <- function() { on.exit(trace <<- c(trace, "inner")); stop("boom") }
outer_call <- function() { on.exit(trace <<- c(trace, "outer")); inner_call() }
for (i in 1:3) {
  trace <- character()
  cat(conditionMessage(tryCatch(outer_call(), error = identity)), paste(trace, collapse = ","), "")
}
cat("\n")

level3 <- function() stop("deep error")
level2 <- function() level3()
level1 <- function() level2()
for (i in 1:3) cat(tryCatch(level1(), error = conditionMessage), "")
cat("\n")

with_restart <- function(x) withRestarts(
  { warning("w"); "no restart" },
  recover = function() paste("recovered", x)
)
for (i in 1:3) {
  cat(withCallingHandlers(with_restart(i), warning = function(w) invokeRestart("recover")), "")
}
cat("\n")

force_it <- function(v) v
looping <- function() {
  total <- 0L
  for (i in 1:10) {
    total <- total + force_it(if (i %% 2L == 0L) next else i)
    if (i >= 7L) break
  }
  total
}
for (i in 1:3) cat(looping(), "")
cat("\n")

seen <- function(x) {
  list(sys.call(), match.call(), deparse(substitute(x)), missing(x), sys.call(-1L))
}
reporter <- function(y) seen(y + 1L)
for (i in 1:3) print(reporter(2L))

parentage <- function() list(deparse(sys.call(-1L)), identical(parent.frame(), middle_env))
middle_env <- NULL
middle <- function() { middle_env <<- environment(); parentage() }
for (i in 1:3) print(middle())

invisible(NULL)
