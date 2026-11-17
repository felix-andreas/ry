# bench(label, f) times f(i). It doubles the number of calls until one batch takes 0.3 s of CPU
# time, then prints the label and the nanoseconds per call of the last batch. With the environment
# variable UNDER_HANDLER=1, the calls run inside withCallingHandlers(), as the code of a test does.
under_handler <- nzchar(Sys.getenv("UNDER_HANDLER"))
run <- function(n, f) for (i in seq_len(n)) f(i)
bench <- function(label, f) {
  n <- 64
  repeat {
    start <- sum(proc.time()[1:2])
    if (under_handler) withCallingHandlers(run(n, f), warning = function(w) NULL) else run(n, f)
    seconds <- sum(proc.time()[1:2]) - start
    if (seconds >= 0.3) break
    n <- n * 2
  }
  cat(sprintf("%s\t%.0f\n", label, 1e9 * seconds / n))
}

# Pattern: plain function bodies. Most application code is functions of 5 to 40 lines that check
# their arguments, read fields, branch, loop over a few elements and call other small functions.
# No single builtin dominates such a body. The cost is the interpreter itself: calls, argument
# matching, promises, variable lookup, assignments and branches.
f0 <- function() NULL
f1 <- function(a) a
f3 <- function(a, b, c) a
fd <- function(a, b = 2, c = a + b) c
outer1 <- function(a, b, c) f3(a, b, c)
chain5 <- function(a) chain4(a)
chain4 <- function(a) chain3(a)
chain3 <- function(a) chain2(a)
chain2 <- function(a) chain1(a)
chain1 <- function(a) a
with_missing <- function(a, b) if (missing(b)) a else b
early_return <- function(a) {
  if (a > 0) return(a)
  -a
}
inner_closure <- function(a) {
  add <- function(v) v + a
  add(1)
}
apply_closure <- function(a) lapply(1:3, function(v) v + a)
guarded <- function(a, kind = "one") {
  if (!is.numeric(a)) stop("a must be numeric")
  if (length(kind) != 1L) stop("kind must have length 1")
  a
}
with_dots <- function(a, ...) f3(a, ...)
field <- function(x, name) x[[name]]

record <- list(id = "k1", kind = "b", size = 3, tags = c("t1", "t2"), inner = list(low = 1, high = 9))
values <- c(2.5, 1.5, 4, 8, 16, 32, 64, 128, 256, 512)

scalar_block <- function(a) {
  b <- a + 1
  c <- b * 2
  d <- c - a
  e <- d / 3
  f <- e + b
  g <- f * f
  h <- g - c
  k <- h + d
  m <- k * 0.5
  m + a
}
branch_chain <- function(kind) {
  if (kind == "a") 1 else if (kind == "b") 2 else if (kind == "c") 3 else 4
}
branch_switch <- function(kind) switch(kind, a = 1, b = 2, c = 3, 4)
read_fields <- function(x) {
  if (is.null(x$tags) || length(x$tags) == 0L) return(0)
  x$size + x$inner$low + x[["inner"]][["high"]]
}
write_fields <- function(x, v) {
  x$size <- v
  x[["kind"]] <- "c"
  x$inner$low <- v
  x
}
loop_sum <- function(v) {
  total <- 0
  for (k in seq_along(v)) total <- total + v[k]
  total
}
loop_fill <- function(n) {
  out <- numeric(n)
  for (k in seq_len(n)) out[k] <- k * 2
  out
}
copy_fields <- function(x) {
  out <- list()
  for (name in names(x)) out[[name]] <- x[[name]]
  out
}
build_record <- function(i) list(id = paste0("k", i), kind = "a", size = i, tags = character(0))

# A helper of the usual shape: checks, a branch, field reads, a short loop and calls of helpers.
summarize_record <- function(x, scale = 1, kinds = c("a", "b", "c"), verbose = FALSE) {
  if (is.null(x)) return(NULL)
  if (!is.list(x)) stop("x must be a list")
  kind <- field(x, "kind")
  if (!(kind %in% kinds)) stop("unknown kind")
  weight <- branch_chain(kind)
  size <- x$size * scale
  bounds <- x$inner
  width <- bounds$high - bounds$low
  total <- 0
  for (tag in x$tags) {
    if (nchar(tag) > 1L) total <- total + weight
  }
  if (verbose) message("total ", total)
  result <- list(id = x$id, size = size, width = width, total = total)
  if (width > 100) result$wide <- TRUE
  result
}

bench("body: f() with no argument", function(i) f0())
bench("body: f(a) with one argument", function(i) f1(i))
bench("body: f(a, b, c) by position", function(i) f3(i, 2, 3))
bench("body: f(a, b = , c = ) by name", function(i) f3(i, c = 3, b = 2))
bench("body: f(a) with two default arguments", function(i) fd(i))
bench("body: a function that passes 3 arguments on", function(i) outer1(i, 2, 3))
bench("body: a chain of 5 calls", function(i) chain5(i))
bench("body: a function that tests missing(b)", function(i) with_missing(i))
bench("body: a function that returns early", function(i) early_return(i))
bench("body: a function that defines and calls a function", function(i) inner_closure(i))
bench("body: lapply over 3 with a function that reads a local", function(i) apply_closure(i))
bench("body: two argument guards with stop()", function(i) guarded(i))
bench("body: a function that passes ... on", function(i) with_dots(i, 2, 3))
bench("body: field(x, name) as x[[name]]", function(i) field(record, "size"))
bench("body: 10 scalar assignments", function(i) scalar_block(i))
bench("body: if / else if chain on a string", function(i) branch_chain("c"))
bench("body: switch() on a string", function(i) branch_switch("c"))
bench("body: reads of x$a, x$a$b, x[['a']][['b']]", function(i) read_fields(record))
bench("body: writes of x$a, x[['a']], x$a$b", function(i) write_fields(record, i))
bench("body: for loop that sums 10 elements", function(i) loop_sum(values))
bench("body: for loop that fills 10 elements", function(i) loop_fill(10L))
bench("body: for loop that copies 5 list fields", function(i) copy_fields(record))
bench("body: list() of 4 named fields with paste0()", function(i) build_record(i))
bench("body: helper of 20 lines", function(i) summarize_record(record, scale = 2))
