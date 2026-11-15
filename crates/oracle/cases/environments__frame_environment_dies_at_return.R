# A frame that hands out its environment while it runs (to a callee asking for its parent
# frame, to `sys.function`, to a promise) and returns with nothing left that names it, beside
# frames whose environment something keeps: the kept ones must still hold every variable.
kept <- list()
peek <- function() parent.frame()
count_names <- function() length(ls(parent.frame()))
borrow <- function(x) { local_value <- x * 2L; count_names() + local_value }
keep <- function(x) { local_value <- x * 2L; kept[[length(kept) + 1L]] <<- peek(); local_value }
give <- function(x) { local_value <- x * 2L; peek() }
choose_kind <- function(kind = c("first", "second")) { kind <- match.arg(kind); kind }
own_function <- function(x) { f <- sys.function(); length(formals(f)) + x }
drive <- function(n) {
  total <- 0L
  last <- NULL
  for (i in seq_len(n)) {
    total <- total + borrow(i) + keep(i) + own_function(i)
    last <- give(i)
    choose_kind()
  }
  list(total, get("local_value", envir = last), sort(ls(last)))
}
str(drive(30L))
print(length(kept))
print(vapply(kept[c(1L, 15L, 30L)], function(e) get("local_value", envir = e), 0L))
print(sort(ls(kept[[30L]])))
# A promise over the frame that the callee forces, and one it keeps unforced.
delayed <- list()
force_it <- function(p) p
hold_it <- function(p) { delayed[[length(delayed) + 1L]] <<- function() p; invisible(NULL) }
promises <- function(n) {
  out <- 0L
  for (i in seq_len(n)) {
    v <- i * 3L
    out <- out + force_it(function() v)()
    hold_it(v + 1L)
  }
  out
}
print(promises(20L))
print(delayed[[20L]]())
caller_side <- function(n) {
  out <- 0L
  for (i in seq_len(n)) {
    inner <- function(x) { y <- x + 1L; hold_it(y * 2L); count_names() }
    out <- out + inner(i)
  }
  out
}
print(caller_side(20L))
print(delayed[[40L]]())
# The frame's own value is its environment, or a closure or a formula over it.
returns_env <- function(x) { inside <- x; count_names(); environment() }
returns_closure <- function(x) { inside <- x; count_names(); function() inside }
returns_formula <- function(x) { inside <- x; count_names(); ~ inside }
collect <- function(n) {
  out <- list()
  for (i in seq_len(n)) out <- list(returns_env(i), returns_closure(i), returns_formula(i))
  list(get("inside", envir = out[[1L]]), out[[2L]](), get("inside", envir = environment(out[[3L]])))
}
str(collect(20L))
# A callee registers exit code on its caller, which then runs when the caller returns.
defer <- function(expr, envir = parent.frame()) do.call(on.exit, list(substitute(expr), add = TRUE), envir = envir)
deferred <- function(n) {
  for (i in seq_len(n)) {
    run <- function(x) { defer(cat("left", x, "\n")); count_names(); x }
    if (i > n - 2L) run(i)
  }
  invisible(NULL)
}
deferred(12L)
