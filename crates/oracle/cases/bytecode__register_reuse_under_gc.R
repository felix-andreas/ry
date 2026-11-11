# Collection on every allocation, inside compiled functions whose registers a deeper call that
# already returned used before them. The values the deeper call left behind are garbage, and
# each collection must neither keep a reader of them alive nor trip over them.
deep <- function(n) {
  words <- paste0("w", seq_len(n))
  upper <- lapply(words, toupper)
  sizes <- vapply(upper, nchar, 1L)
  list(words, upper, sum(sizes), rev(words), paste(words, collapse = ""))
}
middle <- function(n) length(deep(n))
wide <- function(k) {
  x <- k + 1
  y <- c(x, x * 2, x * 3)
  z <- paste(y, collapse = ",")
  w <- list(x, y, z, nchar(z), rev(y), sum(y), toupper(z), y[[2]] + y[[3]])
  c(length(w), w[[4]], w[[6]], w[[8]])
}
finalized <- 0L
make_tracked <- function() {
  e <- new.env()
  reg.finalizer(e, function(e) finalized <<- finalized + 1L)
  invisible(NULL)
}
results <- list()
gctorture(TRUE)
for (i in 1:6) {
  middle(i + 3)
  results[[i]] <- wide(i)
}
gctorture(FALSE)
results
# A collection inside a function as deep as the one that dropped the environments, so its
# registers start where theirs were, still finalizes every one of them.
collect_here <- function() {
  before <- finalized
  invisible(gc())
  finalized - before
}
for (i in 1:3) make_tracked()
collect_here()
for (i in 1:3) make_tracked()
collect_here()
finalized
