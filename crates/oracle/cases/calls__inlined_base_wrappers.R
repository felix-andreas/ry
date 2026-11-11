# Calls of base wrappers whose `.Internal` the compiler inlines beyond GNU R's own "safe base
# internals": `paste` and `paste0`, which forward `...` as `list(...)`, `sprintf`, which passes `...`
# on as the internal's own arguments, and the `vector` family, which passes a literal mode. Inside
# loops that tier 0 and the JIT compile, `...` must take the arguments R's matching gives it, in
# order, and the internal must see base's namespace, not the caller's frame, when it dispatches.
call_of <- function(expr) tryCatch(expr, error = function(e) print(list(conditionMessage(e), conditionCall(e))))
warned <- function(expr) withCallingHandlers(expr, warning = function(w) { print(list(conditionMessage(w), conditionCall(w))); invokeRestart("muffleWarning") })
strings <- function(n) {
  out <- list()
  a <- "x"
  for (i in seq_len(n)) out <- list(paste(a, i), paste(a, i, sep = "-"), paste(sep = "+", a, i, collapse = "|"),
    paste(), paste(a, character(0)), paste(a, character(0), recycle0 = TRUE), paste(first = a, i),
    paste0(a, i), paste0(a, 1:i, collapse = ","), paste0(collapse = NULL, "y", i),
    sprintf("%s=%d", a, i), sprintf(fmt = "%05.1f", pi * i), sprintf("%s", list("l", i)), sprintf("plain"),
    character(i), logical(), numeric(i), integer(2L), double(1), raw(i),
    startsWith(c("ab", "b"), "a"), endsWith("ab", c("b", "a")), make.unique(c("a", "a", "b")),
    make.unique(c("a", "a"), sep = "_"), .row_names_info(data.frame(x = seq_len(i))),
    .row_names_info(data.frame(x = 1), type = 2L), isNamespace(baseenv()), isNamespace(asNamespace("stats")))
  out
}
for (round in 1:3) str(strings(2))
# Arguments are evaluated as the call would force them: `...` first, then the formals after it.
log <- character()
note <- function(v) { log <<- c(log, v); v }
ordered <- function() { log <<- character(); r <- paste(note("a"), sep = note("s"), note("b")); list(r, log) }
for (round in 1:3) str(ordered())
# A tag that partially matches a formal before `...` names that formal, as `f` names `fmt`.
partial <- function() { out <- character(); for (i in 1:2) out[i] <- sprintf(f = "%d", i); out }
for (round in 1:3) print(partial())
# A method or a function named `as.character` in the caller is not what `paste` dispatches to,
# since the wrapper's frame encloses base's namespace; a global method is.
classed <- structure(1, class = "wrapped")
dispatch <- function() {
  as.character.wrapped <- function(x, ...) "local method"
  as.character <- function(x, ...) "local function"
  out <- character()
  for (i in 1:2) out[i] <- paste("p", classed)
  out
}
for (round in 1:3) print(dispatch())
as.character.wrapped <- function(x, ...) "global method"
for (round in 1:3) print(dispatch())
rm(as.character.wrapped)
# Errors and warnings name the call to the wrapper.
bad_collapse <- function() for (i in 1:2) paste(i, collapse = NA)
bad_format <- function() for (i in 1:2) sprintf("%d", i + 0.5)
unused <- function() { for (i in 1:2) r <- sprintf("%s", i, "extra"); r }
bad_length <- function() for (i in 1:2) character(-i)
for (round in 1:3) {
  call_of(bad_collapse()); call_of(bad_format()); print(warned(unused())); call_of(bad_length())
}
# Rebinding `paste` or `sprintf` after the sites were compiled reaches the new definition.
joined <- function() { out <- character(); for (i in 1:2) out[i] <- paste0("v", i); out }
formatted <- function() { out <- character(); for (i in 1:2) out[i] <- sprintf("%d", i); out }
for (round in 1:3) { print(joined()); print(formatted()) }
paste0 <- function(...) "global paste0"
sprintf <- function(fmt, ...) "global sprintf"
print(joined()); print(formatted())
rm(paste0, sprintf)
print(joined()); print(formatted())
pass_dots <- function(...) { out <- character(); for (i in 1:2) out[i] <- paste(..., i); out }
print(pass_dots("d", "e"))
