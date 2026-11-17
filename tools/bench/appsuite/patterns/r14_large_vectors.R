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

# Pattern: the same operations as in the other files, but on vectors and tables with thousands
# of elements. Here the time goes into the builtin or into the C code of a package, and not into
# the interpreter.
library(data.table)
setDTthreads(1)

n <- 5000
a <- paste0("name", seq_len(n))
b <- rep(c("x", "y", "z", "w", "v"), n / 5)
keys <- paste(a, b, sep = "|")
shuffled <- keys[order((seq_len(n) * 7919) %% n)]
days <- as.Date("2015-01-01") + seq_len(n) - 1
numbers <- as.numeric(seq_len(n))
values <- as.list(numbers)
flags <- rep(c(TRUE, FALSE), n / 2)
p <- seq(0.001, 0.999, length.out = n)

make <- function(i) data.table(day = days[1:100] + i, id = paste0("id", i, "_", 1:100))
parts <- lapply(1:200, make)
big1 <- data.table(day = days, id = keys, value = numbers, kind = b)
big2 <- copy(big1)
file <- tempfile(fileext = ".rds")
saveRDS(big1, file)

bench("large: paste(a, b, sep = '|') of 5,000 strings", function(i) paste(a, b, sep = "|"))
bench("large: paste0('id', 1:5000)", function(i) paste0("id", seq_len(n)))
bench("large: 5,000 strings %in% 5,000 strings", function(i) shuffled %in% keys)
bench("large: match() of 5,000 strings", function(i) match(shuffled, keys))
bench("large: unique() of 5,000 strings", function(i) unique(b))
bench("large: order() of 5,000 strings", function(i) order(shuffled))
bench("large: which() of 5,000 logicals", function(i) which(flags))
bench("large: x[logical index] of 5,000 numbers", function(i) numbers[flags])
bench("large: unlist() of a list of 5,000 numbers", function(i) unlist(values))
bench("large: lapply() over 5,000 numbers", function(i) lapply(numbers, function(x) x + 1))
bench("large: as.POSIXlt() of 5,000 dates", function(i) as.POSIXlt(days))
bench("large: format() of 5,000 dates", function(i) format(days))
bench("large: as.Date() of 5,000 texts", function(i) as.Date(c("2024-01-31", "2024-02-29")[rep(1:2, n / 2)]))
bench("large: ifelse() on 5,000 values", function(i) ifelse(is.na(numbers), 0, numbers * 2))
bench("large: qt() of 5,000 probabilities, df = 4.5", function(i) qt(p, 4.5))
bench("large: Reduce(rbind) over 200 tables of 100 rows", function(i) Reduce(rbind, parts))
bench("large: rbindlist() of 200 tables of 100 rows", function(i) rbindlist(parts))
bench("large: rbindlist() of two tables of 5,000 rows", function(i) rbindlist(list(big1, big2)))
bench("large: unique() of a table of 5,000 rows", function(i) unique(big1))
bench("large: table[kind == 'x'] of 5,000 rows", function(i) big1[kind == "x"])
bench("large: table[, sum(value), by = kind]", function(i) big1[, sum(value), by = kind])
bench("large: merge() of two tables of 5,000 rows", function(i) merge(big1, big2, by = "id"))
bench("large: setkey() on a text column", function(i) setkey(copy(big1), id))
bench("large: serialize() of a table of 5,000 rows", function(i) serialize(big1, NULL))
bench("large: unserialize() of that table", function(i) unserialize(serialize(big1, NULL)))
bench("large: saveRDS() and readRDS() of that table", function(i) { saveRDS(big1, file); readRDS(file) })
bench("large: copy of a numeric vector, one element set", function(i) { x <- numbers; x[1] <- i; x })
bench("large: sum(), cumsum() and diff() of 5,000 numbers", function(i) sum(cumsum(diff(numbers))))
