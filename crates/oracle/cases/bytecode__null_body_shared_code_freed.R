# A closure whose body is `NULL`, made afresh at each call from one `function`
# expression, shares its compiled body with the other closures over that body.
# Once every such closure has died, a full collection frees the code, and a new
# closure must compile again rather than run the freed code.
handler <- quote(function(e) NULL)
make <- function() eval(handler)
for (i in 1:50) stopifnot(is.null(make()(i)))
for (round in 1:20) {
  invisible(gc(full = TRUE))
  junk <- lapply(1:2000, function(i) function(x) x + i)
  rm(junk)
  stopifnot(is.null(make()(round)))
}
cat("ok\n")
