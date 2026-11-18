# tryCatch() with zero, one and several handlers: which handler runs, what the frames of
# tryCatchList(), tryCatchOne() and doTryCatch() show from the expression and from a handler,
# the values that pass through the chain, and the frames that outlive the call.
below <- (function() sys.nframe())() - 1L
stack <- function() {
  n <- sys.nframe() - 1L - below
  for (i in seq_len(n)) {
    cat(i, sys.parents()[[below + i]] - below, deparse(sys.call(below + i))[[1L]], "\n")
  }
}
at <- function(i) sys.frame(below + i)
innermost <- function() sys.frame(sys.nframe() - 1L)
variables <- function() {
  for (i in seq_len(sys.nframe() - 1L - below)) {
    cat(i, sort(ls(sys.frame(below + i), all.names = TRUE)), "\n")
  }
}

cat("-- no handler\n")
print(tryCatch(1 + 1))
tryCatch(stack())
tryCatch(variables())
tryCatch(variables(), finally = cat("done\n"))
r <- tryCatch(at(2L)$nh)
print(r)

cat("-- one handler\n")
tryCatch(stack(), error = function(e) NULL)
tryCatch(variables(), error = function(e) NULL)
tryCatch(stop("s"), error = function(e) stack())
tryCatch(stop("s"), error = function(e) variables())
tryCatch(warning("w"), warning = function(w) variables())
r <- tryCatch(at(3L)$name, error = function(e) NULL)
print(r)
r <- tryCatch((function() sys.function(below + 4L))(), error = function(e) NULL)
print(names(formals(r)))
print(body(r))
r <- tryCatch(stop("s"), error = function(e) at(3L)$value[[2L]])
print(r)
r <- tryCatch(stop("s"), error = function(e) class(at(3L)$cond))
print(r)

cat("-- three handlers\n")
three <- function(code) {
  tryCatch(
    code,
    error = function(e) paste("error:", conditionMessage(e)),
    warning = function(w) paste("warning:", conditionMessage(w)),
    message = function(m) paste("message:", conditionMessage(m))
  )
}
print(three(stop("a")))
print(three(warning("b")))
print(three(message("c")))
print(three("none"))
three(stack())
three(variables())
tryCatch(warning("w"), error = function(e) NULL, warning = function(w) stack(), message = function(m) NULL)
tryCatch(warning("w"), error = function(e) NULL, warning = function(w) variables(), message = function(m) NULL)
level <- function(i) c(at(i + 1L)$nh, at(i + 2L)$name)
r <- three(c(level(2L), level(5L), level(8L)))
print(r)
r <- three(vapply(c(3L, 6L, 9L), function(i) paste(at(i)$names, collapse = "+"), ""))
print(r)
r <- three(vapply(c(3L, 6L, 9L), function(i) length(at(i)$handlers), 0L))
print(r)

cat("-- a handler signals\n")
print(tryCatch(stop("a"), error = function(e) stop("b"), condition = function(c) paste("later handler:", conditionMessage(c))))
r <- tryCatch(
  tryCatch(stop("a"), condition = function(c) stop("b"), error = function(e) "earlier handler"),
  error = function(e) paste("outer call:", conditionMessage(e))
)
print(r)
print(tryCatch(stop("a"), error = function(e) warning("from handler"), warning = function(w) conditionMessage(w)))
print(tryCatch(tryCatch(stop("a"), error = function(e) stop(e)), error = function(e) conditionMessage(e)))

cat("-- handlers that are not closures\n")
print(tryCatch(stop("a"), error = conditionMessage))
print(class(tryCatch(stop("a"), error = identity)))
print(tryCatch(stop("a"), error = class))
print(tryCatch(tryCatch(stop("a"), error = 1), error = function(e) conditionMessage(e)))
print(tryCatch(tryCatch(stop("a"), error = 1), error = function(e) deparse(conditionCall(e))))
print(tryCatch(tryCatch(stop("a"), function(e) 1), error = function(e) conditionMessage(e)))
print(tryCatch(tryCatch(1, stop("handler argument")), error = function(e) conditionMessage(e)))

cat("-- the value and its visibility\n")
tryCatch(invisible(1), error = function(e) NULL)
tryCatch(1, error = function(e) NULL)
tryCatch(stop("a"), error = function(e) invisible(2))
tryCatch(stop("a"), error = function(e) 3)
value <- 4
tryCatch(value, error = function(e) NULL)
tryCatch(value <- 5, error = function(e) NULL)
print(value)
tryCatch(invisible(6), finally = cat("finally\n"))
tryCatch(invisible(7))
through <- function(x) tryCatch(x, error = function(e) "caught")
print(through(8))
print(through(stop("a")))
print(tryCatch(through(), error = function(e) conditionMessage(e)))
print(tryCatch(tryCatch(, error = function(e) "inner"), error = function(e) conditionMessage(e)))

cat("-- jumps through the chain\n")
returns <- function() {
  tryCatch(return("from the expression"), error = function(e) NULL, finally = cat("finally\n"))
  "after"
}
print(returns())
returns <- function() {
  tryCatch(stop("a"), error = function(e) return("from the handler"))
  "after"
}
print(returns())
for (i in 1:4) {
  tryCatch({
    if (i == 2L) next
    if (i == 4L) break
    cat("iteration", i, "\n")
  }, error = function(e) NULL, warning = function(w) NULL)
}
print(withRestarts(tryCatch(invokeRestart("leave", 9), error = function(e) "handler"), leave = function(x) x + 1))
print(tryCatch(withCallingHandlers(stop("a"), error = function(e) cat("calling first\n")), error = function(e) "exiting"))
counted <- 0
depth <- function(n) if (n == 0L) stop("bottom") else tryCatch(depth(n - 1L), warning = function(w) NULL, finally = counted <<- counted + 1)
print(tryCatch(depth(40L), error = function(e) conditionMessage(e)))
print(counted)

cat("-- frames that outlive the call\n")
frame <- tryCatch(innermost(), error = function(e) NULL)
print(sort(ls(frame)))
print(frame$name)
print(class(frame$handler))
print(environmentName(frame$parentenv))
kept <- tryCatch(lapply(1:4, at), error = function(e) NULL, warning = function(w) NULL)
print(lapply(kept, function(env) sort(ls(env))))
print(kept[[2L]]$nh)
print(kept[[3L]]$name)
print(names(kept[[1L]]$handlers))
print(identical(parent.env(kept[[4L]]), kept[[3L]]))
print(identical(parent.env(kept[[3L]]), kept[[1L]]))
print(identical(environment(kept[[1L]]$tryCatchList), kept[[1L]]))
print(identical(environment(kept[[3L]]$doTryCatch), kept[[3L]]))
maker <- tryCatch(function() "made inside", error = function(e) NULL)
print(maker())

cat("-- a simpleError bound in the tryCatchOne frame\n")
replaced <- tryCatch(
  {
    assign("simpleError", function(...) "replaced", at(3L))
    1 + "a"
  },
  error = function(e) e
)
print(replaced)
cat("-- the return(expr) promise of a finished call\n")
finished <- tryCatch(at(4L), error = function(e) NULL)
print(tryCatch(finished$expr, warning = function(w) conditionMessage(w)))

cat("-- many calls\n")
total <- 0
for (i in 1:20000) {
  total <- total + tryCatch(
    {
      x <- numeric(20)
      if (i %% 7L == 0L) stop("e") else if (i %% 11L == 0L) warning("w") else i
    },
    error = function(e) -1,
    warning = function(w) -2
  )
}
print(total)
nested <- function(n) if (n == 0L) 0 else tryCatch(nested(n - 1L) + 1, error = function(e) NA)
print(nested(200L))
