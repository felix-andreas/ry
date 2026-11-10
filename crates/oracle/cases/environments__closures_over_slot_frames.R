# A closure defined inside a function whose frame keeps its variables in slots: handlers that
# never run, handlers that run, closures that escape by return, by `<<-`, by a list and by an
# option, the closure's environment compared with the defining frame, `sys.function` and
# `parent.frame` inside a handler, `environment<-`, and a frame that makes more closures than are
# tracked. Each runs a few times so compiled code runs it too.
quiet <- function(x) {
  tryCatch(x + 1, error = function(e) "failed", warning = function(w) "warned")
}

handled <- function(x) {
  tryCatch(stop("boom ", x), error = function(e) paste("caught", conditionMessage(e), x))
}

adder <- function(n) {
  offset <- n * 2
  function(x) x + offset
}

later <- function(n) {
  value <- n
  h <- function() value
  value <- n + 100
  h
}

listed <- function(n) {
  list(get = function() n, double = function() 2 * n)
}

keeper <- NULL
keep <- function(n) {
  local_value <- n + 1
  keeper <<- function() local_value
  invisible(NULL)
}

same_env <- function() {
  f <- function() NULL
  identical(environment(f), sys.frame(sys.nframe()))
}

same_env_late <- function() {
  f <- function() NULL
  frame <- environment()
  identical(environment(f), frame)
}

read_env <- function(n) {
  inner <- n * 3
  f <- function() NULL
  get("inner", envir = environment(f))
}

inside_handler <- function() {
  withCallingHandlers(
    {
      warning("w")
      "done"
    },
    warning = function(w) {
      fun <- sys.function()
      cat("handler formals:", names(formals(fun)), "\n")
      cat("parent is a frame:", is.environment(parent.frame()), "\n")
      invokeRestart("muffleWarning")
    }
  )
}

rebind <- function(n) {
  secret <- n
  f <- function() secret
  environment(f) <- list2env(list(secret = "replaced"))
  f
}

many <- function(n) {
  fs <- lapply(seq_len(n), function(i) function() i * 10)
  vapply(fs, function(f) f(), numeric(1))
}

many_kept <- function() {
  a1 <- function() 1; a2 <- function() 2; a3 <- function() 3; a4 <- function() 4
  a5 <- function() 5; a6 <- function() 6; a7 <- function() 7; a8 <- function() 8
  a9 <- function() 9; a10 <- function() 10; a11 <- function() 11; a12 <- function() 12
  a13 <- function() 13; a14 <- function() 14; a15 <- function() 15; a16 <- function() 16
  a17 <- function() 17; a18 <- function() 18
  list(a1, a18)
}

via_option <- function(n) {
  token <- paste0("opt", n)
  options(ry.closure.test = function() token)
  NULL
}

for (round in 1:3) {
  cat("round", round, "\n")
  print(quiet(round))
  print(quiet("a"))
  print(handled(round))
  print(adder(round)(10))
  print(later(round)())
  l <- listed(round)
  print(c(l$get(), l$double()))
  keep(round)
  print(keeper())
  print(same_env())
  print(same_env_late())
  print(read_env(round))
  print(inside_handler())
  print(rebind(round)())
  print(many(3))
  k <- many_kept()
  print(c(k[[1]](), k[[2]]()))
  via_option(round)
  print(getOption("ry.closure.test")())
}
