#@ weight 3
#@ param SIZE int 3 40
#@ param SCALE int 2 9
# A memoised computation backed by an in-memory cache, with a call counter for tests.
#: @trust list{count: integer}
cache_calls_@ID@ <- local({
  calls <- new.env()
  calls$count <- 0L
  calls
})

#: fn(n: double) -> double
expensive_@ID@ <- function(n) {
  cache_calls_@ID@$count <- cache_calls_@ID@$count + 1L
  sum(seq_len(n) * @SCALE@)
}

#: fn([cache]: cachem) -> fn(n: double) -> double
memoised_@ID@ <- function(cache = cachem::cache_mem(max_size = 1024 * 1024)) {
  memoise::memoise(expensive_@ID@, cache = cache)
}

#: fn(...: Any) -> character
cache_key_@ID@ <- function(...) {
  #: character
  digest::digest(list(...), algo = "xxhash64")
}

#: fn(fn: fn(n: double) -> double, [sizes]: double[]) -> double[]
warm_cache_@ID@ <- function(fn, sizes = seq_len(@SIZE@)) {
  vapply(sizes, fn, numeric(1))
}
#@ test
test_that("memoised computation @ID@ computes once per argument", {
  cache_calls_@ID@$count <- 0L
  fn <- memoised_@ID@()
  expect_equal(fn(@SIZE@), sum(seq_len(@SIZE@)) * @SCALE@)
  fn(@SIZE@)
  fn(@SIZE@)
  expect_equal(cache_calls_@ID@$count, 1L)
  expect_equal(cache_key_@ID@(1, "a"), cache_key_@ID@(1, "a"))
  expect_false(identical(cache_key_@ID@(1, "a"), cache_key_@ID@(2, "a")))
  expect_length(warm_cache_@ID@(fn), @SIZE@)
})
