# Opening a file that is not there, and load() on files that are not saved images.
show <- function(expr) {
  result <- withVisible(tryCatch(
    withCallingHandlers(expr, warning = function(w) {
      cat("warning:", gsub("'[^']*/([a-z]+)[0-9a-f]*'", "'\\1'", gsub("‘[a-z0-9]+’", "‘FILE’", conditionMessage(w))), "\n")
      invokeRestart("muffleWarning")
    }),
    error = function(e) {
      cat("error:", conditionMessage(e), "\n")
      invisible(NULL)
    }
  ))
  if (result$visible) print(result$value)
}
g <- tempfile("nofile")
show(gzfile(g, "rb"))
show(open(gzfile(g), "r"))
show(readLines(gzfile(g)))
show(bzfile(g, "rb"))
show(xzfile(g, "rb"))
show(file(g, "rb"))
show(gzfile(file.path(g, "sub"), "wb"))
show(bzfile(file.path(g, "sub"), "wb"))
show(xzfile(file.path(g, "sub"), "wb"))
show(readRDS(g))
show(load(g))
h <- tempfile("img")
writeLines("list(a = 1, 2)", h)
show(load(h))
show(readRDS(h))
show(unserialize(readBin(h, "raw", 100)))
writeLines("RDX9\nabc", h)
show(load(h))
writeLines("RD", h)
show(load(h))
writeLines("RDZ7\nabc", h)
show(load(h))
writeBin(raw(0), h)
show(load(h))
show(load(1))
show(load(character(0)))
closeAllConnections()
