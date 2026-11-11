# oracle: requires R6
# An R6 object a deeper call dropped runs its finalize method at a gc() inside a later function
# with a large register window.
log <- new.env()
log$count <- 0L
Tracked <- R6::R6Class("Tracked",
  public = list(id = NULL, initialize = function(id) self$id <- id),
  private = list(finalize = function() log$count <- log$count + 1L)
)
deeper <- function(n) {
  for (i in seq_len(n)) Tracked$new(i)
  invisible(NULL)
}
wide <- function(k) {
  before <- log$count
  invisible(gc())
  after <- log$count
  a <- k + 1; b <- a * 2; c <- b - a; d <- list(a, b, c); e <- paste(a, b, c)
  f <- nchar(e); g <- c(a, b, c, f); h <- rev(g); i <- sum(h); j <- toupper(e)
  c(after - before, length(list(d, g, h, i, j)))
}
deeper(2)
wide(1)
deeper(3)
wide(2)
log$count
