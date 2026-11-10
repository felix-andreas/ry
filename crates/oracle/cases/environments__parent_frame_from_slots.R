# parent.frame() of a caller that keeps its variables in slots: passed on, compared, read,
# written, kept past the caller's return, and used as a target for exit code and return().
defer <- function(expr, envir = parent.frame(), after = FALSE) {
  thunk <- as.call(list(function() expr))
  do.call(base::on.exit, list(thunk, TRUE, after), envir = envir)
}
local_note <- function(label, envir = parent.frame()) {
  defer(cat("cleanup", label, "\n"), envir = envir)
  invisible(label)
}
uses_defer <- function(x) {
  local_note("a")
  local_note("b")
  defer(cat("x was", x, "\n"), after = TRUE)
  x * 2
}
print(uses_defer(21))

same_frame <- function() {
  here <- environment()
  child <- function() identical(parent.frame(), here)
  child()
}
print(same_frame())

compares <- function() {
  probe <- function(env = parent.frame()) identical(env, globalenv())
  c(probe(), probe(globalenv()))
}
print(compares())

reads_caller <- function(name) get(name, envir = parent.frame())
reader <- function() {
  secret <- 42
  reads_caller("secret")
}
print(reader())

writes_caller <- function() assign("made", "by callee", envir = parent.frame())
writer <- function() {
  made <- "before"
  writes_caller()
  made
}
print(writer())

kept <- NULL
keep_caller <- function() kept <<- parent.frame()
keeper <- function() {
  local_value <- "still here"
  keep_caller()
  invisible(NULL)
}
keeper()
print(get("local_value", envir = kept))

returns_caller <- function() parent.frame()
returner <- function() {
  inside <- "inside"
  returns_caller()
}
print(get("inside", envir = returner()))

boxed <- function() list(env = parent.frame())
boxer <- function() {
  token <- "boxed"
  boxed()
}
print(get("token", envir = boxer()$env))

child_env <- function() new.env(parent = parent.frame())
parenting <- function() {
  outer <- "outer value"
  child_env()
}
print(get("outer", envir = parenting()))

early <- function() {
  jumper <- function() eval(quote(return("returned early")), parent.frame())
  jumper()
  "not reached"
}
print(early())

two_up <- function() parent.frame(2)
middle <- function() two_up()
top <- function() {
  mark <- "top"
  get("mark", envir = middle())
}
print(top())

caught <- function() {
  value <- tryCatch(stop("boom"), error = function(e) conditionMessage(e))
  paste("caught", value)
}
print(caught())

handled <- function() {
  withCallingHandlers(
    {
      warning("careful")
      "finished"
    },
    warning = function(w) {
      cat("saw", conditionMessage(w), "\n")
      invokeRestart("muffleWarning")
    }
  )
}
print(handled())

evaluates <- function() {
  x <- "local"
  helper <- function(code) eval(substitute(code), parent.frame())
  helper(paste(x, "seen"))
}
print(evaluates())

called_from <- function() sys.function(sys.parent())
caller_fn <- function() {
  f <- called_from()
  identical(f, caller_fn)
}
print(caller_fn())

do_called <- function() do.call(paste, list("a", "b"), envir = parent.frame())
print((function() do_called())())
