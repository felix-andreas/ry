# A compiled loop keeps its scalar variables unboxed and writes them back to the frame before a
# call that could observe them. The write-back may store into the box the variable already holds
# when nothing else references it, so every way R code can keep the old value (another variable,
# a list, a forced promise, an argument still being evaluated, `mget`) must keep seeing it.
# Each function runs a few times so compiled code runs it too.
ticks <- 0
tick <- function() ticks <<- ticks + 1

kept <- function(n) {
  lo <- 0L
  half <- 0
  flag <- FALSE
  saved <- NULL
  listed <- list()
  for (i in 1:n) {
    lo <- lo + 1L
    half <- i / 2
    flag <- i %% 2L == 0L
    tick()
    if (i == 3L) {
      saved <- lo
      saved_half <- half
      saved_flag <- flag
    }
    if (i %% 4L == 0L) listed[[length(listed) + 1L]] <- c(lo, half)
  }
  list(saved, saved_half, saved_flag, listed, lo, half, flag)
}

promised <- function(n) {
  lo <- 0L
  for (i in 1:n) {
    lo <- lo + 1L
    tick()
    if (i == 2L) forced <- (function(x) { force(x); function() x })(lo)
    if (i == 2L) lazy <- (function(x) function() x)(lo)
  }
  c(forced(), lazy(), lo)
}

in_flight <- function(n) {
  lo <- 0L
  out <- list()
  for (i in 1:n) {
    lo <- lo + 1L
    tick()
    out[[i]] <- c(lo, { lo <- lo + 10L; tick(); lo })
  }
  out
}

snapshot <- function(n) {
  lo <- 0L
  frame <- environment()
  for (i in 1:n) {
    lo <- lo + 1L
    tick()
    if (i == 2L) copy <- mget("lo", envir = frame)
  }
  list(copy, lo)
}

formal <- function(a, b = 5L) {
  before <- c(missing(a), missing(b))
  for (i in 1:3) {
    a <- i
    b <- b + i
    tick()
  }
  list(before, missing(a), missing(b), a, b)
}

for (round in 1:3) {
  cat("round", round, "\n")
  str(kept(9L))
  print(promised(4L))
  str(in_flight(3L))
  str(snapshot(4L))
  str(formal())
  str(formal(1L, 2L))
}
print(ticks)
