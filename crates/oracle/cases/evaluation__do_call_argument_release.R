# A value passed to a compiled closure that hands it on through `do.call` is collectable once the
# caller drops it. Tier 0 compiles a closure after its first calls, so the later passes run
# compiled code, and every pass must finalize its own environment at its own gc().
select <- function(frame, ...) list(...)
checksum <- function(answer) {
  sums <- lapply(c("a", "b"), function(name) name)
  length(unlist(as.list(do.call(select, c(list(answer), sums)))))
}
for (pass in 1:6) {
  answer <- new.env()
  reg.finalizer(answer, function(e) cat("  finalize", pass, "\n"))
  total <- checksum(answer)
  rm(answer)
  invisible(gc())
  cat("after gc", pass, "\n")
}

# The same inside a function that is itself called often enough to compile.
run <- function(pass) {
  answer <- new.env()
  reg.finalizer(answer, function(e) cat("  finalize run", pass, "\n"))
  total <- checksum(answer)
  rm(answer)
  invisible(gc())
  cat("after run gc", pass, "\n")
  total
}
for (pass in 1:4) run(pass)
