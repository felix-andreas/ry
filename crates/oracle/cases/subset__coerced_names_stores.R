# A test user's reproducer: names set from numbers are found by `[[`, `[` and `$`, and a store
# through `[[<-` or `$<-` replaces the named element instead of appending a duplicate. Each
# route is read at top level and inside a function that runs often enough to compile.
routes <- list(
  integer = function() c(1L, 2L),
  double = function() c(1, 2),
  as_character = function() as.character(1:2),
  format = function() format(c(1, 2)),
  literal = function() c("1", "2"),
  paste0 = function() paste0(1:2)
)
reads <- function(l) list(l[["2"]], unname(l["2"]), l$`2`)
stores <- function(l) {
  a <- l
  a[["2"]] <- "by [[<-"
  b <- l
  b$`2` <- "by $<-"
  b$`3` <- "new"
  list(a, b)
}
for (route in names(routes)) {
  l <- list(1, 2)
  names(l) <- routes[[route]]()
  cat("==", route, "\n")
  str(reads(l))
  str(stores(l))
  for (i in 1:300) {
    r <- reads(l)
    s <- stores(l)
  }
  stopifnot(identical(r, reads(l)), identical(s, stores(l)))
}

# setNames with numeric ids, read back by as.character(id), as real code keys objects.
ids <- c(7, 42, 1000, 123456)
objects <- setNames(as.list(sprintf("object %d", seq_along(ids))), ids)
fetch <- function(objects, ids) vapply(ids, function(id) objects[[as.character(id)]], "")
print(fetch(objects, ids))
for (i in 1:300) fetched <- fetch(objects, ids)
print(fetched)
objects[[as.character(42)]] <- "replaced"
objects$`7` <- "replaced by $"
str(objects)
counts <- setNames(integer(3), c(1L, 2L, 3L))
for (id in c(1L, 3L, 3L, 2L, 3L)) counts[[as.character(id)]] <- counts[[as.character(id)]] + 1L
print(counts)
