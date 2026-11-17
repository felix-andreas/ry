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

# Pattern: small data frames (10 rows) are built, bound together row by row, and read and
# changed column by column.
days <- as.Date("2024-01-01") + 0:9
make <- function(i) data.frame(col1 = paste0("k", 1:10), col2 = days, col3 = as.numeric(1:10) + i, stringsAsFactors = FALSE)
df <- make(0)
parts <- lapply(1:5, make)

bench("data.frame: data.frame() with 3 columns", function(i) make(i))
bench("data.frame: rbind() of 2 frames", function(i) rbind(parts[[1]], parts[[2]]))
bench("data.frame: do.call(rbind, 5 frames)", function(i) do.call(rbind, parts))
bench("data.frame: df[['col3']]", function(i) df[["col3"]])
bench("data.frame: df$col3", function(i) df$col3)
bench("data.frame: df$col4 <- value", function(i) { x <- df; x$col4 <- i; x })
bench("data.frame: df[df$col1 == 'k3', ]", function(i) df[df$col1 == "k3", ])
bench("data.frame: df[, c('col1', 'col3')]", function(i) df[, c("col1", "col3")])
bench("data.frame: df[3, 'col3']", function(i) df[3, "col3"])
bench("data.frame: nrow() and colnames()", function(i) nrow(df) + length(colnames(df)))
bench("data.frame: as.data.frame() of a list", function(i) as.data.frame(list(a = 1:10, b = letters[1:10])))
bench("data.frame: merge() on one key", function(i) merge(parts[[1]], parts[[2]], by = "col1"))
bench("data.frame: df[order(df$col3), ]", function(i) df[order(-df$col3), ])
bench("data.frame: cbind() of a frame and a vector", function(i) cbind(df, col5 = 1:10))
bench("data.frame: apply() over the rows of a matrix", function(i) apply(matrix(1:40, nrow = 10), 1, sum))
