# An object dropped in a function body is collectable at the next gc(), on every call, after the
# body has compiled too.
k <- function(i) {
  e <- new.env()
  reg.finalizer(e, function(x) cat("  finalize", i, "\n"))
  rm(e)
  cat("gc in", i, "\n")
  invisible(gc())
  cat("after gc", i, "\n")
}
for (i in 1:12) k(i)

# A method copied into another object with environment<- shares its compiled body, and the copy
# must not keep the original's environment alive.
make <- function(label) {
  self <- new.env()
  self$label <- label
  self$get <- function() self$label
  environment(self$get) <- self
  reg.finalizer(self, function(e) cat("  finalize", e$label, "\n"))
  self
}
a <- make("a")
for (i in 1:12) a$get()
b <- make("b")
b$get <- a$get
environment(b$get) <- b
for (i in 1:12) b$get()
rm(a)
invisible(gc())
cat("after rm(a)\n")
rm(b)
invisible(gc())
cat("after rm(b)\n")
