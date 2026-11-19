# Names made by coercion (integer, double, logical, factor levels, as.character, paste0,
# sprintf, format, deparse) must be found by every by-name read, exactly as literal names
# are: equal strings built by different routes are one name.
routes <- list(
  integer = function(n) seq_len(n),
  double = function(n) as.double(seq_len(n)),
  as_character = function(n) as.character(seq_len(n)),
  paste0 = function(n) paste0("", seq_len(n)),
  sprintf = function(n) sprintf("%d", seq_len(n)),
  format = function(n) format(seq_len(n), trim = TRUE),
  deparse = function(n) vapply(seq_len(n), deparse, ""),
  factor = function(n) levels(factor(seq_len(n)))
)
reads <- function(x, key) {
  c(
    double_bracket = deparse(x[[key]]),
    subset2 = deparse(.subset2(x, key)),
    single = deparse(unname(x[key])),
    match = match(key, names(x)),
    in_names = key %in% names(x),
    switch = deparse(switch(key, "1" = "one", "2" = "two", "3" = "three", "none"))
  )
}
for (route in names(routes)) {
  l <- list(10, 20, 30)
  names(l) <- routes[[route]](3)
  v <- c(10, 20, 30)
  names(v) <- routes[[route]](3)
  cat(route, "\n")
  print(reads(l, "2"))
  print(reads(l, as.character(3L)))
  print(reads(v, "2"))
  cat("dollar:", deparse(l$`2`), deparse(l$"3"), "\n")
  key <- as.character(1L)
  cat("computed key:", l[[key]], v[[key]], "\n")
}

# setNames on vectors and lists.
s <- setNames(as.list(letters[1:3]), 1:3)
print(s[["3"]])
print(setNames(1:3, c(TRUE, FALSE, NA))[["FALSE"]])
print(setNames(list(1, 2), c(1.5, 2.5))[["2.5"]])
print(setNames(c(a = 1, b = 2), 7:8)[["8"]])

# Logical names.
lg <- list(1, 2)
names(lg) <- c(TRUE, FALSE)
print(lg[["FALSE"]])
print(lg$`TRUE`)

# A store by a coerced name replaces the element instead of appending a new one.
st <- list(1, 2)
names(st) <- 1:2
st[["2"]] <- "replaced"
st$`1` <- "also replaced"
str(st)
vs <- c(1, 2)
names(vs) <- 1:2
vs[["2"]] <- 99
print(vs)

# A list keyed by as.character(id), read in a loop that gets hot.
by_id <- list()
ids <- c(3L, 17L, 42L, 1000L)
for (id in ids) by_id[[as.character(id)]] <- id * 2L
lookup <- function(table, ids) {
  total <- 0L
  for (round in 1:200) for (id in ids) total <- total + table[[as.character(id)]]
  total
}
print(lookup(by_id, ids))
keyed <- setNames(as.list(ids * 2L), ids)
print(lookup(keyed, ids))
