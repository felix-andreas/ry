# A hashed environment lists in the order of GNU R's chained hash table, which depends on the
# whole history of assignments and removals: rm() never shrinks the table, and its chain count
# falls only when a bucket empties.
h <- new.env(hash = TRUE)
for (i in 1:60) assign(paste0("w", i), i, envir = h)
rm(list = paste0("w", 1:45), envir = h)
ls(h, sorted = FALSE)
profile <- env.profile(h)
profile$size
profile$nchains
which(profile$counts > 0)
for (i in 1:30) assign(paste0("z", i), i, envir = h)
ls(h, sorted = FALSE)
env.profile(h)$nchains
rm(list = ls(h), envir = h)
ls(h, sorted = FALSE)
env.profile(h)[c("size", "nchains")]
assign("a", 1, envir = h)
assign("b", 2, envir = h)
rm("a", envir = h)
assign("a", 3, envir = h)
names(as.list(h))
small <- new.env(size = 5L)
for (name in c("x", "y", "q", "r", "s", "t", "u")) {
  assign(name, 1, envir = small)
  if (name == "q") rm("x", envir = small)
}
ls(small, sorted = FALSE)
env.profile(small)[c("size", "nchains")]
