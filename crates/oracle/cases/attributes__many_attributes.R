# `attr<-` and `attr()` on a value with many attributes: an existing one is replaced where it
# stands, a new one is appended, a read matches exactly before partially, a miss is `NULL`,
# and `NULL` removes one.
x <- integer(3)
for (j in 1:300) attr(x, paste0("key", j)) <- j
attr(x, "key150") <- -150L
attr(x, "fresh") <- "new"
print(attr(x, "key150"))
print(attr(x, "key1"))
print(attr(x, "fre"))
print(attr(x, "missing"))
attr(x, "key2") <- NULL
print(length(attributes(x)))
print(head(names(attributes(x)), 4))
print(tail(names(attributes(x)), 3))
counts <- function(words) {
  freqs <- integer(0)
  for (w in words) {
    cnt <- attr(freqs, w)
    attr(freqs, w) <- if (is.null(cnt)) 1L else cnt + 1L
  }
  unlist(attributes(freqs))
}
print(counts(c("gg", "ta", "gg", "ac", "ta", "gg", "g")))
