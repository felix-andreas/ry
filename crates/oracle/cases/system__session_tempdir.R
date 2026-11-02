# The session's temporary directory is a fresh Rtmp directory that forked children share and
# tempdir(check = TRUE) creates again after it is deleted.
d <- tempdir()
cat(grepl("/Rtmp[A-Za-z0-9]{6}$", d), dir.exists(d), "\n")
f <- tempfile()
writeLines("x", f)
cat(dirname(f) == d, "\n")
children <- parallel::mclapply(1:2, function(i) tempdir(), mc.cores = 2)
cat(all(unlist(children) == d), dir.exists(d), "\n")
unlink(d, recursive = TRUE)
cat(dir.exists(tempdir()), dir.exists(tempdir(check = TRUE)), "\n")
