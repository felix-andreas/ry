# `x[] <- value` and `x[a:b] <- value` write whole copies of `value` over a run of elements; the
# result, its attributes, the recycling rules and the sharing must match one-at-a-time writes.
show <- function(value) { str(value); cat(deparse(attributes(value)), "\n") }
attempt <- function(expr) tryCatch(expr, error = function(e) cat("error:", conditionMessage(e), "\n"),
                                   warning = function(w) cat("warning:", conditionMessage(w), "\n"))

# Every vector type, with a value of the same type, shorter and of full length.
doubles <- c(a = 1.5, b = 2.5, c = NA, d = 4, e = 5, f = 6)
x <- doubles; x[] <- 0; show(x)
x <- doubles; x[] <- c(10, 20); show(x)
x <- doubles; x[] <- c(1, 2, 3, 4, 5, NaN); show(x)
integers <- 1:6 + 0L
x <- integers; x[] <- 9L; show(x)
x <- integers; x[] <- c(7L, NA, 8L); show(x)
flags <- c(TRUE, FALSE, NA, TRUE)
x <- flags; x[] <- c(FALSE, NA); show(x)
strings <- c(a = "p", b = NA, c = "", d = "long string here")
x <- strings; x[] <- "z"; show(x)
x <- strings; x[] <- c("u", NA); show(x)
x <- strings; x[] <- rev(strings); show(x)
items <- list(1L, "two", NULL, c(3, 4), list(5), quote(sym))
x <- items; x[] <- list("only"); show(x)
x <- items; x[] <- list(NULL, 1:2, quote(f(y))); show(x)
x <- items; x[] <- rev(items); show(x)
calls <- expression(a + b, f(x), 1, sym)
x <- calls; x[] <- expression(g(1), 2); show(x)
complexes <- c(1+2i, NA, 3i); x <- complexes; x[] <- c(0+1i); show(x)
bytes <- as.raw(1:4); x <- bytes; x[] <- as.raw(c(255, 0)); show(x)

# A value of another type converts the vector or the value, as an indexed write does.
x <- doubles; x[] <- 1L; show(x)
x <- doubles; x[] <- NA; show(x)
x <- integers; x[] <- 2.5; show(x)
x <- integers; x[] <- "s"; show(x)
x <- flags; x[] <- 1:2; show(x)
x <- strings; x[] <- 1:2; show(x)
x <- doubles; x[] <- list(1, "a"); show(x)
x <- items; x[] <- 1:3; show(x)
x <- items; x[] <- "s"; show(x)

# Lengths that do not divide, empty values and empty targets.
attempt({ x <- doubles; x[] <- c(1, 2, 3, 4); show(x) })
attempt({ x <- doubles; x[] <- numeric(0); show(x) })
attempt({ x <- strings; x[] <- character(0); show(x) })
attempt({ x <- items; x[] <- list(); show(x) })
x <- numeric(0); x[] <- 1; show(x)
x <- numeric(0); x[] <- numeric(0); show(x)
x <- character(0); x[] <- c("a", "b"); show(x)
x <- list(); x[] <- list(1, 2); show(x)
x <- NULL; x[] <- 1; show(x)

# Matrices, arrays and classed objects keep their attributes.
m <- matrix(as.numeric(1:12), 3, dimnames = list(letters[1:3], LETTERS[1:4]))
x <- m; x[] <- 0; show(x)
x <- m; x[] <- m * 2; show(x)
x <- m; x[] <- c(1, 2, 3); show(x)
x <- m; x[] <- matrix(as.numeric(12:1), 4); show(x)
a <- array(1:24, 2:4); x <- a; x[] <- 24:1; show(x)
f <- factor(c("u", "v", "u")); x <- f; attempt({ x[] <- "v"; show(x) })
x <- f; attempt({ x[] <- "w"; show(x) })
d <- as.Date("2024-01-02") + 0:2; x <- d; x[] <- as.Date("2025-05-05"); show(x)
s <- structure(c(1, 2, 3), class = "tagged", note = "kept"); x <- s; x[] <- c(9, 8, 7); show(unclass(x))
frame <- data.frame(p = 1:3, q = c("a", "b", "c")); x <- frame; x[] <- list(3:1, c("z", "y", "x")); show(x)
x <- frame; x[] <- 0L; show(x)
ts1 <- ts(c(1, 2, 3, 4), start = 2020, frequency = 4); x <- ts1; x[] <- 4:1 + 0; show(x)

# The value is the vector itself, or shares its elements.
x <- doubles; x[] <- x; show(x)
x <- strings; x[] <- x; show(x)
x <- items; x[] <- x; show(x)
x <- items; x[] <- x[c(2, 1)]; show(x)
x <- strings; y <- x; x[] <- "changed"; show(y); show(x)
x <- items; y <- x; x[] <- list(0); show(y); show(x)
x <- m; y <- x; x[] <- -1; show(y)
fill <- function(v, value) { v[] <- value; v }
x <- doubles; y <- fill(x, 0); show(x); show(y)
x <- items; y <- fill(x, list("f")); show(x); show(y)
value <- c("a", "b"); x <- strings; x[] <- value; x[1] <- "first"; show(value)
value <- list(1, 2, 3); x <- items; x[] <- value; x[[1]] <- "first"; show(value)

# A run `a:b` inside the vector, at its ends, reversed, empty and past the end.
x <- doubles; x[2:4] <- 0; show(x)
x <- doubles; x[1:6] <- c(1, 2, 3); show(x)
x <- doubles; x[3:6] <- c(7, 8); show(x)
x <- doubles; x[6:6] <- 60; show(x)
x <- doubles; x[4:2] <- c(40, 30, 20); show(x)
x <- strings; x[2:3] <- c("m", "n"); show(x)
x <- strings; x[1:4] <- "all"; show(x)
x <- items; x[2:5] <- list("a", NULL); show(x)
x <- items; x[1:2] <- items[5:6]; show(x)
x <- integers; x[2:5] <- 0L; show(x)
x <- flags; x[1:2] <- NA; show(x)
x <- m; x[2:7] <- 0; show(x)
x <- doubles; x[5:8] <- c(50, 60, 70, 80); show(x)
x <- strings; x[4:6] <- c("d", "e", "f"); show(x)
x <- doubles; x[seq_len(0)] <- 1; show(x)
x <- doubles; attempt({ x[0:2] <- c(1, 2); show(x) })
x <- doubles; attempt({ x[2:4] <- c(1, 2); show(x) })
x <- doubles; attempt({ x[2:4] <- numeric(0); show(x) })
x <- doubles; x[seq(2, 4)] <- c(-1, -2, -3); show(x)
x <- doubles; x[2:4 + 0] <- c(-1, -2, -3); show(x)
x <- doubles; x[c(2, 3, 4)] <- 0; show(x)
x <- items; x[3:4] <- x[1:2]; show(x)
x <- strings; x[1:2] <- x[3:4]; show(x)

# Inside a loop, where the assignment is compiled, and with a large value.
grow <- numeric(8)
for (step in 1:3) { grow[] <- step; grow[2:3] <- c(step * 10, step * 20) }
show(grow)
labels <- character(6)
for (step in 1:3) { labels[] <- c("a", "b"); labels[3:4] <- paste0("s", step) }
show(labels)
big <- numeric(2e5); big[] <- as.numeric(1:1e5); cat(sum(big), big[1e5], big[1e5 + 1], "\n")
bigs <- character(1e5); bigs[] <- c("a", "bb"); cat(sum(nchar(bigs)), bigs[99999:100000], "\n")
bigl <- vector("list", 1e5); bigl[] <- list(1L, "x"); cat(sum(lengths(bigl)), class(bigl[[99999]]), "\n")
for (round in 1:3) { bigs[] <- paste0("r", round); invisible(gc()) }
cat(bigs[1], bigs[1e5], "\n")
