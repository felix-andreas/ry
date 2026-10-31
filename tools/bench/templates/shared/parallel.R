#@ weight 2
#@ param CHUNKS int 2 8
#@ ns importFrom(foreach,"%do%")
# The same reduction through foreach, parallel and purrr, which must agree.
chunk_sums_@ID@ <- function(x, chunks = @CHUNKS@L) {
  groups <- split(x, rep_len(seq_len(chunks), length(x)))
  foreach::foreach(g = groups, .combine = c) %do% sum(g)
}

forked_sums_@ID@ <- function(x, chunks = @CHUNKS@L) {
  groups <- split(x, rep_len(seq_len(chunks), length(x)))
  unlist(parallel::mclapply(groups, sum, mc.cores = 1L), use.names = FALSE)
}

mapped_sums_@ID@ <- function(x, chunks = @CHUNKS@L) {
  groups <- split(x, rep_len(seq_len(chunks), length(x)))
  purrr::map_dbl(groups, sum) |> unname()
}

with_workers_@ID@ <- function(expr) {
  doParallel::registerDoParallel(cores = 1L)
  on.exit(foreach::registerDoSEQ(), add = TRUE)
  force(expr)
}

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
