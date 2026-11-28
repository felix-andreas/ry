# Forcing an argument of a base closure with a native entry can give the calling frame an
# environment between the entry's lookups, and a later name is then read from that frame.
g0 <- 1
f <- function(x) identical(x, g0)
for (i in 1:3) print(f({ assign("g0", 2, envir = sys.frame(1)); 2 }))
h <- function(x) identical(x, g0)
for (i in 1:3) print(h({ invisible(sys.frames()); 1 }))
print(g0)
