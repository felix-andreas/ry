# A hashed frame lists in GNU R's bucket order, which depends on every size its table has had,
# so removals (which empty buckets and trigger compaction here) must not disturb the replay.
set.seed(1)
for (trial in 1:60) {
  e <- new.env(hash = TRUE, size = sample(c(1L, 5L, 29L), 1))
  n <- sample(5:120, 1)
  for (i in seq_len(n)) {
    nm <- paste0(sample(c(letters, LETTERS, "."), sample(1:8, 1), TRUE), collapse = "")
    assign(nm, i, envir = e)
    if (runif(1) < 0.1) { l <- ls(e, all.names = TRUE); rm(list = sample(l, 1), envir = e) }
  }
  p <- env.profile(e)
  cat(trial, p$size, p$nchains, ":", ls(e, sorted = FALSE, all.names = TRUE), "\n")
}

# A complex assignment binds `*tmp*` for its length, which moves the table's counter.
e <- new.env(hash = TRUE, size = 5L)
for (nm in c("v", "w", "x", "y")) assign(nm, 1:3, envir = e)
for (k in 1:6) {
  local(v[2] <- 0L, envir = e)
  p <- env.profile(e)
  cat(k, p$size, p$nchains, ":", ls(e, sorted = FALSE), "\n")
}
