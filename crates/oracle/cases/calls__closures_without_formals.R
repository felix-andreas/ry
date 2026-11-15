# Calls of closures that take no arguments, from sites that run often enough to plan.
zero <- function() 42L
where <- function() sort(ls(parent.frame()))
who <- function() list(sys.call(), deparse(sys.function()), nargs())
counter <- 0L
bump <- function() { counter <<- counter + 1L; invisible(counter) }
loop <- function(n) {
  total <- 0L
  local_name <- "here"
  for (i in seq_len(n)) { total <- total + zero(); bump() }
  list(total, where(), who())
}
str(loop(25L))
print(counter)
# One site calls closures of different shapes: with and without formals, made anew on every
# pass, and one that a later definition replaces.
shapes <- function(n) {
  out <- character(0)
  for (i in seq_len(n)) {
    f <- if (i %% 3L == 0L) function() "none" else if (i %% 3L == 1L) function(x = "default") x else function(...) paste(nargs())
    out <- c(out, f())
  }
  out
}
print(shapes(12L))
replaced <- function(n) { out <- integer(0); for (i in seq_len(n)) out <- c(out, zero()); out }
print(replaced(10L))
zero <- function() 7L
print(replaced(3L))
zero <- function(unused) 8L
print(replaced(3L))
# An argument passed to a closure without formals is an error, also once the site has a plan.
strict <- function(n) {
  out <- character(0)
  for (i in seq_len(n)) out <- c(out, tryCatch(if (i > n - 2L) bump(i) else as.character(bump()), error = function(e) conditionMessage(e)))
  out
}
print(strict(12L))
# `on.exit`, `Recall`, `return()` and an error inside the callee.
depth <- 0L
down <- function() { depth <<- depth + 1L; if (depth < 30L) Recall() else depth }
print(down())
tidy <- function() { on.exit(cat("tidied\n")); invisible("value") }
early <- function() { for (i in 1:10) if (i == 4L) return(i); 0L }
fails <- function() stop("nothing to do")
runner <- function(n) {
  out <- list()
  for (i in seq_len(n)) out <- list(tidy(), early(), tryCatch(fails(), error = function(e) conditionCall(e)))
  out
}
str(runner(10L))
# A closure without formals that asks for its frame, keeps it, or returns it.
frames <- list()
self <- function() environment()
maker <- function() { hidden <- "inside"; function() hidden }
holder <- function(n) {
  for (i in seq_len(n)) { e <- self(); frames[[i]] <<- e; g <- maker() }
  list(length(ls(e)), g())
}
str(holder(10L))
print(length(unique(frames)))
