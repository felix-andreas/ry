# Calls of base closures whose body is only `.Internal(f(...))` (`match`, `identical`, `typeof`,
# `nchar` and the rest of GNU R's "safe base internals"), inside loops that tier 0 and the JIT
# compile to the internal called directly behind a guard. Each rebinding route must reach the new
# definition: a global or attached definition, `assignInNamespace` or `unlockBinding` plus `assign`
# on base, and a rebinding after the call site was compiled.
call_of <- function(expr) tryCatch(expr, error = function(e) print(list(conditionMessage(e), conditionCall(e))))
wrappers <- function(n) {
  out <- list()
  for (i in seq_len(n)) out <- list(typeof(i), inherits(i, "integer"), inherits(what = "numeric", x = i), identical(i, 2L),
    match(i, c(3L, 2L, 1L)), match(table = 1:2, x = i), rep_len(i, 3), rep.int(i, 2), vector("list", i), vector(),
    which.max(c(1, i, 0)), which.min(c(1, -i)), choose(5, i), atan2(i, 1), drop(matrix(i)), is.vector(i), lchoose(5, i), beta(i, 2),
    nchar(c("ab", NA)), nchar("ab", "bytes"))
  out
}
for (round in 1:3) str(wrappers(2))
pass_dots <- function(...) typeof(...); print(pass_dots(1L))
invalid <- "a\xffb"
strict <- function(s) { out <- integer(length(s)); for (i in seq_along(s)) out[i] <- nchar(s[[i]], "chars", FALSE); out }
bad_length <- function(n) { for (i in 1:2) r <- rep_len(1:3, n); r }
for (round in 1:3) {
  call_of(strict(c("ok", invalid))); call_of(nchar(invalid, "chars", FALSE))
  call_of(rep_len(1:3, -1)); call_of(bad_length(-1)); call_of(nchar("a", "nope"))
}
finds <- function(xs) { out <- integer(length(xs)); for (k in seq_along(xs)) out[k] <- match(xs[k], c("a", "b", "c")); out }
same <- function(xs) { out <- logical(length(xs)); for (k in seq_along(xs)) out[k] <- identical(xs[k], "b"); out }
for (round in 1:3) { print(finds(c("b", "z", "a"))); print(same(c("b", "z"))) }
# A global definition made after the sites were compiled.
match <- function(x, table, ...) -1L
identical <- function(x, y, ...) NA
print(finds(c("b", "z"))); print(same(c("b", "z")))
rm(match, identical)
print(finds(c("b", "z"))); print(same(c("b", "z")))
# A definition on an attached environment, in front of base on the search path.
attach(list(match = function(x, table, ...) -2L, identical = function(x, y, ...) "attached"), name = "shadows")
print(finds(c("b", "z"))); print(same(c("b", "z")))
detach("shadows")
print(finds(c("b", "z"))); print(same(c("b", "z")))
# A definition local to the calling function, and one its enclosure gains later.
local_shadow <- function(xs) { match <- function(x, table, ...) -3L; out <- integer(length(xs)); for (k in seq_along(xs)) out[k] <- match(xs[k], "a"); out }
for (round in 1:3) print(local_shadow(c("a", "b")))
maker <- function() { inner <- function(xs) { out <- integer(length(xs)); for (k in seq_along(xs)) out[k] <- match(xs[k], "a"); out }; list(run = inner, shadow = function() assign("match", function(x, table, ...) -4L, envir = environment(inner))) }
closure <- maker()
for (round in 1:3) print(closure$run(c("a", "b")))
closure$shadow(); print(closure$run(c("a", "b")))
# Base's own binding replaced, through assignInNamespace and through unlockBinding plus assign.
# GNU R's byte compiler assumes base's bindings never change and keeps calling the original
# internal, while its interpreter calls the replacement. ry's guard always reaches the replacement,
# so GNU R is compared with its JIT off; ry still runs these loops compiled.
invisible(compiler::enableJIT(0))
original_match <- base::match
original_identical <- base::identical
assignInNamespace("match", function(x, table, nomatch = NA_integer_, incomparables = NULL) -5L, "base")
finds_new <- function(xs) { out <- integer(length(xs)); for (k in seq_along(xs)) out[k] <- match(xs[k], c("a", "b", "c")); out }
for (round in 1:3) print(finds_new(c("b", "z")))
assignInNamespace("match", original_match, "base")
unlockBinding("identical", baseenv())
assign("identical", function(x, y, num.eq = TRUE, single.NA = TRUE, attrib.as.set = TRUE, ignore.bytecode = TRUE, ignore.environment = FALSE, ignore.srcref = TRUE, extptr.as.ref = FALSE) "replaced", envir = baseenv())
same_new <- function(xs) { out <- logical(length(xs)); for (k in seq_along(xs)) out[k] <- identical(xs[k], "b"); out }
for (round in 1:3) print(same_new(c("b", "z")))
assign("identical", original_identical, envir = baseenv())
lockBinding("identical", baseenv())
print(finds(c("b", "z"))); print(same(c("b", "z")))
