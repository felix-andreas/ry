#@ weight 2
#@ param CHUNKS int 2 8
#@ ns importFrom(foreach,"%do%")
# The same reduction through foreach, parallel and purrr, which must agree.
#: fn(x: integer[], [chunks]: integer) -> integer[]
chunk_sums_@ID@ <- function(x, chunks = @CHUNKS@L) {
  groups <- split(x, rep_len(seq_len(chunks), length(x)))
  foreach::foreach(g = groups, .combine = c) %do% {
    #: @if-unknown integer[]
    group <- g
    sum(group)
  }
}

#: fn(x: integer[], [chunks]: integer) -> integer[]
forked_sums_@ID@ <- function(x, chunks = @CHUNKS@L) {
  groups <- split(x, rep_len(seq_len(chunks), length(x)))
  unlist(parallel::mclapply(groups, sum, mc.cores = 1L), use.names = FALSE)
}

#: fn(x: integer[], [chunks]: integer) -> double[]
mapped_sums_@ID@ <- function(x, chunks = @CHUNKS@L) {
  groups <- split(x, rep_len(seq_len(chunks), length(x)))
  purrr::map_dbl(groups, sum) |> unname()
}

#: <T> fn(expr: T) -> T
with_workers_@ID@ <- function(expr) {
  doParallel::registerDoParallel(cores = 1L)
  on.exit(foreach::registerDoSEQ(), add = TRUE)
  force(expr)
}

#: fn(n: integer) -> integer
run_isolated_@ID@ <- function(n) {
  callr::r(function(n) sum(seq_len(n)), args = list(n = n))
}
#@ test
test_that("reductions @ID@ agree across engines", {
  x <- seq_len(100)
  expect_equal(sum(chunk_sums_@ID@(x)), sum(x))
  expect_equal(forked_sums_@ID@(x), mapped_sums_@ID@(x))
  expect_equal(with_workers_@ID@(sum(chunk_sums_@ID@(x))), sum(x))
})
