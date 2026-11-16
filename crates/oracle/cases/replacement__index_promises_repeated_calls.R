`[<-.lazy` <- function(x, i, j, value) x
`sub<-` <- function(x, i, value) x
z <- structure(list(), class = "lazy")
cases <- list(
  "z[expr, j] <- v (S3 method)" = function(z) { z[notDefined == 1, "a"] <- 5; z },
  "z[sym] <- v (S3 method)"     = function(z) { z[notDefined] <- 5; z },
  "sub(z, expr) <- v (closure)" = function(z) { sub(z, notDefined == 1) <- 5; z }
)
for (name in names(cases)) for (n in 1:3)
  cat(sprintf("%-30s call %d: %s\n", name, n, tryCatch({ cases[[name]](z); "ok" }, error = conditionMessage)))
for (n in 1:3) cat(sprintf("%-30s call %d: %s\n", "z[expr, j] <- v (top level)", n, tryCatch({ z[notDefined == 1, "a"] <- 5; "ok" }, error = conditionMessage)))
