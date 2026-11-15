# Compact double sequences (as.double(1:n) keeps the sequence compact, as in GNU R) read
# element by element in a hot loop, by [ and [[, through length(), and as the loop sequence,
# including a decreasing one, an index past the end and a sequence longer than an integer counts.
x <- as.double(1:2000)
state <- function(v) sub("^@[0-9a-f]+ ", "", capture.output(.Internal(inspect(v)))[1])
state(x)
total <- function(x) {
    acc <- 0
    for (i in seq_along(x)) acc <- acc + x[i] * 2 - x[[i]]
    acc
}
for (k in 1:30) r <- total(x)
r
state(x)
down <- as.double(50:1)
for (k in 1:30) r <- total(down)
r
past <- function(x, n) {
    out <- 0
    for (k in 1:n) out <- x[length(x) + 1L]
    out
}
past(x, 500)
loop <- function(x) {
    acc <- 0
    for (v in x) acc <- acc + v
    acc
}
for (k in 1:30) r <- loop(x)
r
long <- function(x) {
    n <- 0
    for (k in 1:300) n <- length(x)
    n
}
huge <- seq_len(3e9)
typeof(huge)
long(huge)
long(x)
