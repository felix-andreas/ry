# bench(label, f) times f(i). It doubles the number of calls until one batch takes 0.3 s of CPU
# time, then prints the label and the nanoseconds per call of the last batch. With the environment
# variable UNDER_HANDLER=1, the calls run inside withCallingHandlers(), as the code of a test does.
under_handler <- nzchar(Sys.getenv("UNDER_HANDLER"))
run <- function(n, f) for (i in seq_len(n)) f(i)
bench <- function(label, f) {
  n <- 64
  repeat {
    start <- sum(proc.time()[1:2])
    if (under_handler) withCallingHandlers(run(n, f), warning = function(w) NULL) else run(n, f)
    seconds <- sum(proc.time()[1:2]) - start
    if (seconds >= 0.3) break
    n <- n * 2
  }
  cat(sprintf("%s\t%.0f\n", label, 1e9 * seconds / n))
}

# Pattern: code looks up records by text keys. It builds the keys with paste() and compares short
# character vectors with %in%, match(), unique() and which().
a <- paste0("name", 1:20)
b <- rep(c("x", "y"), 10)
keys <- paste(a, b, sep = "|")
values <- as.list(seq_along(keys))
names(values) <- keys
flags <- rep(c(TRUE, FALSE), 10)

bench("keys: paste(a, b, sep = '|') of 20 strings", function(i) paste(a, b, sep = "|"))
bench("keys: paste0() of two scalars", function(i) paste0("name", i))
bench("keys: paste(..., collapse = ', ') of 20 strings", function(i) paste(a, collapse = ", "))
bench("keys: scalar %in% 20 strings", function(i) "name7|x" %in% keys)
bench("keys: 20 strings %in% 20 strings", function(i) keys %in% a)
bench("keys: match() of a scalar", function(i) match("name7|x", keys))
bench("keys: unique() of 20 strings", function(i) unique(b))
bench("keys: which() of 20 logicals", function(i) which(flags))
bench("keys: names(which(named logicals))", function(i) names(which(c(a = TRUE, b = FALSE, d = TRUE))))
bench("keys: list element by name x[[key]]", function(i) values[["name7|x"]])
bench("keys: subset a named list by 5 names", function(i) values[keys[1:5]])
bench("keys: unlist(lapply(list, names))", function(i) unlist(lapply(list(values[1:5], values[6:10]), names), use.names = FALSE))
bench("keys: setdiff() of short vectors", function(i) setdiff(a, a[1:5]))
bench("keys: sprintf() with two values", function(i) sprintf("%s_%d", "name", i))
bench("keys: gsub() on a scalar", function(i) gsub("|", "_", "name7|x", fixed = TRUE))
bench("keys: nchar() and substr()", function(i) substr("name7|x", 1, nchar("name7|x") - 2))
bench("keys: toupper() and trimws()", function(i) toupper(trimws(" name ")))
