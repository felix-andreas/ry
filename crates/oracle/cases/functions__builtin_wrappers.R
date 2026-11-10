# A wrapper whose body is one builtin call behaves exactly like the closure it is: the
# condition calls, the frame stack seen from handlers, defaults, laziness and rebinding.
# Each driver runs four times so hot sites run planned and pinned.
w_log <- function(x) log(x)
w_braced <- function(x) { log(x) }
crc <- utils:::C_crc64
w_crc <- function(x) .Call(crc, x)
w_round <- function(x, digits = 1) round(x, digits)
w_rev <- function(a, b) c(b, a)
w_unused <- function(x, y) abs(x)
w_sqrt <- function(x) sqrt(x)

show_stack <- function() {
  calls <- sys.calls()
  frames <- sys.parents()
  print(vapply(calls, function(call) deparse(call)[1], ""))
  wrapper <- which(vapply(calls, function(call) is.call(call) && is.symbol(call[[1]]) &&
    startsWith(as.character(call[[1]]), "w_"), NA))
  for (index in wrapper) print(identical(sys.function(index), get(as.character(calls[[index]][[1]]))))
}

drive <- function(round) {
  cat("round", round, "\n")
  # Arguments are variables and literals: a call argument (even `-1`) needs a promise.
  two <- exp(2)
  negative <- -1
  print(w_log(two))
  print(w_braced(1))
  print(w_crc("hello"))
  print(tryCatch(w_log("a"), error = function(e) list(conditionMessage(e), conditionCall(e))))
  print(tryCatch(w_braced("a"), error = function(e) conditionCall(e)))
  print(tryCatch(w_crc(1), error = function(e) list(conditionMessage(e), conditionCall(e))))
  withCallingHandlers(
    w_log(negative),
    warning = function(w) {
      print(conditionCall(w))
      show_stack()
      invokeRestart("muffleWarning")
    }
  )
  tryCatch(
    withCallingHandlers(w_crc(1), error = function(e) show_stack()),
    error = function(e) cat("caught\n")
  )
  print(w_round(2.345))
  print(w_round(2.345, 2))
  print(w_round(digits = 0, x = 2.5))
  print(tryCatch(w_sqrt(), error = function(e) list(conditionMessage(e), conditionCall(e))))
  delayedAssign("p", { cat("forcing p\n"); 1 })
  delayedAssign("q", { cat("forcing q\n"); 2 })
  print(w_rev(p, q))
  delayedAssign("bad", stop("never forced"))
  print(w_unused(-3, bad))
  value <- 4
  print(w_sqrt(value))
}
for (round in 1:4) drive(round)

# A rebinding of the builtin or of `{` where the wrapper looks them up takes effect at once.
again <- function(f, x) for (round in 1:4) print(f(x))
again(w_log, 1)
log <- function(x) "global log"
again(w_log, 1)
rm(log)
again(w_log, 1)
hijacked <- new.env()
assign("{", function(x) "hijacked brace", envir = hijacked)
w_hijacked <- eval(quote(function(x) { abs(x) }), hijacked)
again(w_hijacked, -1)
