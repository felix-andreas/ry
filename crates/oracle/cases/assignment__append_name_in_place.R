# Appending a new name with `$<-`, `[[<-` and `[<-` inside a compiled function, where a local,
# unshared vector grows in place, next to every shape that must still copy: a shared target,
# atomic vectors of every type, vectors without names, other attributes, and NA or empty names.
appends <- function(n) {
  x <- list(a = 1)
  for (i in seq_len(n)) x[[paste0("k", i)]] <- i
  x
}
str(appends(5))

shapes <- function() {
  l <- list(a = 1)
  l$b <- 2
  l[["c"]] <- "three"
  l["d"] <- list(NULL)
  shared <- list(a = 1)
  alias <- shared
  shared$b <- 2
  r <- c(a = 1.5)
  r[["b"]] <- 2.5
  r["c"] <- 3.5
  i <- c(a = 1L)
  i[["b"]] <- 2L
  i["c"] <- 3
  s <- c(a = "x")
  s[["b"]] <- "y"
  lg <- c(a = TRUE)
  lg[["b"]] <- NA
  plain <- 1:2
  plain[["c"]] <- 3L
  plainlist <- list(1, 2)
  plainlist$c <- 3
  attr1 <- structure(list(a = 1), extra = "kept")
  attr1$b <- 2
  dimmed <- structure(list(a = 1), class = "rec")
  dimmed$b <- 2
  empty <- list()
  empty$a <- 1
  nas <- c(a = 1)
  nas[[NA_character_]] <- 2
  blank <- list(a = 1)
  blank[[""]] <- 2
  nulled <- list(a = 1)
  nulled$b <- NULL
  list(l = l, shared = shared, alias = alias, r = r, i = i, s = s, lg = lg, plain = plain,
    plainlist = plainlist, attr1 = attr1, dimmed = unclass(dimmed), empty = empty,
    nas = nas, blank = blank, nulled = nulled)
}
str(shapes())

in_loop <- function() {
  out <- list()
  keep <- list()
  for (name in c("x", "y", "z")) {
    out[[name]] <- nchar(name)
    keep[[length(keep) + 1]] <- out
  }
  list(out = out, keep = keep)
}
str(in_loop())

grow_numeric <- function() {
  v <- c(a = 0)
  for (k in 1:40) v[[sprintf("n%02d", k)]] <- k
  list(length(v), head(names(v), 3), tail(v, 2))
}
str(grow_numeric())

setClass("Holder", representation(items = "list"))
h <- new("Holder", items = list(a = 1))
h@items$b <- 2
str(h@items)
e <- new.env()
e$a <- 1
e[["b"]] <- 2
print(sort(ls(e)))

# Arguments: the promise's value is the caller's, so the callee binds a changed copy.
fee <- function(trade, rate = 0.5) {
  trade$fee <- trade$a * rate
  trade[["net"]] <- trade$a - trade$fee
  trade$a <- 10
  trade[[1]] <- trade[[1]] + 1
  trade
}
base <- list(a = 4)
str(fee(base))
str(base)
str(fee(list(a = 2)))
numbers <- c(a = 1, b = 2)
bump <- function(v) { v[["b"]] <- 20; v[["c"]] <- 30; v }
print(bump(numbers))
print(numbers)
totals <- Reduce(function(acc, k) {
  acc[[k]] <- (if (is.null(acc[[k]])) 0 else acc[[k]]) + nchar(k)
  acc
}, c("x", "yy", "x", "zzz"), list())
str(totals)
lazy <- function(x) { x[["b"]] <- 1; x }
str(lazy(list(a = { cat("forced\n"); 1 })))
