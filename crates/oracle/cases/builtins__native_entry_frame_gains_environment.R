# Forcing an argument of a base closure with a native entry can give the calling frame an
# environment between the entry's lookups, and a later name is then read from that frame.
frame_of <- function(fun) {
  for (n in rev(seq_len(sys.nframe() - 1L))) if (identical(sys.function(n), fun)) return(sys.frame(n))
  stop("not running")
}
g0 <- 1
f <- function(x) identical(x, g0)
for (i in 1:3) print(f({ assign("g0", 2, envir = frame_of(f)); 2 }))
h <- function(x) identical(x, g0)
for (i in 1:3) print(h({ invisible(sys.frames()); 1 }))
# A value read before the forcing stays alive when the forcing rebinds its variable.
k <- function(x) { y <- c(1, 2, 3) * 2; identical(y, x) }
for (i in 1:3) print(k({ assign("y", NULL, envir = frame_of(k)); invisible(gc()); c(2, 4, 6) }))
print(g0)
