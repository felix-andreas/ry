# unlink() in the master before and after forked children that remove trees of their own.
tree <- function() {
  d <- tempfile("fork")
  dir.create(file.path(d, "a", "b"), recursive = TRUE)
  writeLines("x", file.path(d, "a", "f.txt"))
  d
}
first <- tree()
print(unlink(first, recursive = TRUE))
print(file.exists(first))
results <- parallel::mclapply(1:4, function(i) {
  d <- tree()
  c(unlink(d, recursive = TRUE), file.exists(d))
}, mc.cores = 2)
print(results)
last <- tree()
print(unlink(last, recursive = TRUE))
print(file.exists(last))
