# A compiled loop enters a closure it does not inline the way the interpreter does, and the
# callee may read and change the frame of the compiled caller: assign into it, return from it,
# raise a condition its handlers see, or ask for its call.

poke <- function(x, ...) {
  assign("seen", x * 2L, envir = parent.frame())
  if (x > 80L) return(list(sys.call(), sys.call(-1), nargs(), identical(sys.function(-1), caller)))
  x
}
caller <- function(n) {
  s <- 0L
  i <- 0L
  seen <- 0L
  while (i < n) {
    i <- i + 1L
    r <- poke(i)
    if (is.list(r)) return(list(r, seen, s))
    s <- s + r + seen
  }
  s
}
for (k in 1:40) caller(60L)
print(caller(30L))
print(caller(90L))

leave <- function(x, ...) {
  if (x == 7L) eval.parent(quote(return("left early")))
  if (x == 5L) rm("total", envir = parent.frame())
  x
}
stops <- function(n) {
  total <- 0L
  for (i in seq_len(n)) {
    v <- leave(i)
    if (!exists("total", inherits = FALSE)) total <- 1000L
    total <- total + v
  }
  total
}
for (k in 1:40) stops(4L)
print(stops(4L))
print(stops(6L))
print(stops(9L))

signal <- function(x, ...) {
  if (x %% 4L == 0L) warning("fourth: ", x)
  if (x == 11L) stop("eleventh")
  x
}
handled <- function(n) {
  count <- 0L
  total <- 0L
  result <- withCallingHandlers(
    tryCatch({
      for (i in seq_len(n)) total <- total + signal(i)
      total
    }, error = function(e) c(conditionMessage(e), total)),
    warning = function(w) {
      count <<- count + 1L
      invokeRestart("muffleWarning")
    }
  )
  list(result, count)
}
for (k in 1:40) handled(3L)
print(handled(3L))
print(handled(10L))
print(handled(20L))

deep <- function(n, ...) if (n == 0L) sys.nframe() else deep(n - 1L)
depths <- function(n) {
  total <- 0L
  for (i in seq_len(n)) total <- total + deep(i %% 5L)
  total
}
for (k in 1:40) depths(50L)
print(depths(20L))
