# String element reads, `==`/`!=` and `nchar` inside loops, which tier 0 and the JIT compile to their string fast paths, across encodings: ASCII, UTF-8, latin1, bytes, NA_character_, and text equal across encodings.
sh <- function(expr) { r <- tryCatch(expr, error = function(e) paste("E:", conditionMessage(e)), warning = function(w) paste("W:", conditionMessage(w))); print(r) }
utf8 <- "café"
latin1 <- iconv(utf8, "UTF-8", "latin1"); print(Encoding(c(utf8, latin1)))
bytes <- utf8; Encoding(bytes) <- "bytes"
invalid <- "a\xffb"
s <- c("1", "7", NA, "3", utf8, latin1, bytes, "", "a\tb", "中文", invalid, "7")
equal_each <- function(s, y) { out <- logical(length(s)); for (i in seq_along(s)) out[i] <- s[i] == y; out }
equal_each2 <- function(s, y) { out <- logical(length(s)); for (i in seq_along(s)) out[i] <- s[[i]] == y; out }
unequal_each <- function(s, y) { out <- logical(length(s)); for (i in seq_along(s)) out[i] <- s[[i]] != y; out }
count_each <- function(s) { n <- 0L; for (i in seq_along(s)) if (s[i] == "7") n <- n + 1L; n }
for (round in 1:3) {
  print(equal_each(s, "7")); print(equal_each2(s, utf8)); print(unequal_each(s, latin1))
  print(equal_each(s, NA_character_)); print(equal_each2(s, bytes)); print(count_each(s[-3]))
}
pairs <- list(c(utf8, latin1), c(latin1, utf8), c(utf8, bytes), c(bytes, bytes), c(latin1, bytes), c(NA, NA), c("a", NA))
pair_equal <- function(a, b) a == b
for (round in 1:3) for (p in pairs) print(c(pair_equal(p[[1]], p[[2]]), p[[1]] != p[[2]]))
nchars <- function(s, type, allowNA) { out <- integer(length(s)); for (i in seq_along(s)) out[i] <- nchar(s[[i]], type, allowNA); out }
nchars_chars <- function(s) { out <- integer(length(s)); for (i in seq_along(s)) out[i] <- nchar(s[[i]]); out }
nchars_keep <- function(s, keep) { out <- integer(length(s)); for (i in seq_along(s)) out[i] <- nchar(s[i], keepNA = keep); out }
valid <- s[-c(7, 11)]
for (round in 1:3) {
  print(nchars_chars(valid)); print(nchars(valid, "bytes", FALSE)); print(nchars(valid, "width", FALSE))
  print(nchars(s, "chars", TRUE)); print(nchars(s, "bytes", FALSE))
  print(nchars_keep(c("ab", NA), TRUE)); print(nchars_keep(c("ab", NA), FALSE)); print(nchars_keep(c("ab", NA), NA))
  sh(nchars(s, "chars", FALSE)); sh(nchars(c("ok", bytes), "width", FALSE)); sh(nchars_chars(c("ok", invalid)))
  print(nchar(c("\t", "a\001b", "\n", "abc"), "width"))
  print(nchar(NA)); print(nchar(NA_character_)); print(nchar(NA, "width")); print(nchar(NA_character_, "bytes"))
}
sh(nchar(factor("a"))); sh(nchar("a", "nope")); sh(nchar("a", "c")); print(nchar(123L)); print(nchar(TRUE, "b"))
x <- "abc"; f <- function() { for (i in 1:2) r <- nchar(x[[1]], type = "chars"); r }; print(f()); print(f())
nchar <- function(x, ...) -1L; print(nchars_chars(c("a", "bb"))); rm(nchar); print(nchars_chars(c("a", "bb")))
plain <- "abc"; marked <- "abc"; Encoding(marked) <- "UTF-8"; ascii_bytes <- "abc"; Encoding(ascii_bytes) <- "bytes"
utf8_bytes <- utf8; Encoding(utf8_bytes) <- "bytes"
for (round in 1:3) print(c(pair_equal(plain, marked), pair_equal(plain, ascii_bytes), pair_equal(utf8, utf8_bytes), utf8 != utf8_bytes))
call_of <- function(expr) tryCatch(expr, error = function(e) print(list(conditionMessage(e), conditionCall(e))))
strict <- function(s) { out <- integer(length(s)); for (i in seq_along(s)) out[i] <- nchar(s[[i]], "chars", FALSE); out }
for (round in 1:3) { call_of(strict(c("ok", invalid))); call_of(nchar(invalid, "chars", FALSE)) }
wrappers <- function(n) {
  out <- list()
  for (i in seq_len(n)) out <- list(typeof(i), inherits(i, "integer"), inherits(what = "numeric", x = i), identical(i, 2L),
    match(i, c(3L, 2L, 1L)), match(table = 1:2, x = i), rep_len(i, 3), rep.int(i, 2), vector("list", i), vector(),
    which.max(c(1, i, 0)), which.min(c(1, -i)), choose(5, i), atan2(i, 1), drop(matrix(i)), is.vector(i), lchoose(5, i), beta(i, 2))
  out
}
for (round in 1:3) str(wrappers(2))
pass_dots <- function(...) typeof(...); print(pass_dots(1L))
bad_length <- function(n) { for (i in 1:2) r <- rep_len(1:3, n); r }
for (round in 1:3) { call_of(rep_len(1:3, -1)); call_of(bad_length(-1)); call_of(nchar("a", "nope")) }
typeof <- function(x) "masked"; print(wrappers(1)[[1]]); rm(typeof); print(wrappers(1)[[1]])
