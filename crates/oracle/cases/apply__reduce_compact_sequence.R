# `Reduce` over a compact sequence (`a:b`, `seq_len`, `seq_along`) reads the elements as
# `x[[i]]` does, in every direction, with and without `init` and `accumulate`.
add <- function(a, b) a + b
show <- function(value) { str(value); cat(deparse(value), "\n") }

show(Reduce(`+`, 1:10))
show(Reduce(add, 1:10))
show(Reduce(`+`, 1:10, right = TRUE))
show(Reduce(`-`, 1:10))
show(Reduce(`-`, 1:10, right = TRUE))
show(Reduce(`+`, 1:10, 100L))
show(Reduce(`+`, 1:10, 0.5))
show(Reduce(`-`, 1:10, 100L, right = TRUE))
show(Reduce(`+`, 1:10, accumulate = TRUE))
show(Reduce(`+`, 1:10, accumulate = TRUE, right = TRUE))
show(Reduce(`-`, 1:6, 100L, accumulate = TRUE))
show(Reduce(`-`, 1:6, 100L, accumulate = TRUE, right = TRUE))
show(Reduce(function(a, b) paste0(a, b), 1:5))
show(Reduce(function(a, b) paste0(a, b), 1:5, accumulate = TRUE))
show(Reduce(function(a, b) c(a, b), 3:1))
show(Reduce(function(a, b) list(a, b), 1:3))
show(Reduce(function(a, b) list(a, b), 1:3, right = TRUE))

# Sequences that go down, start elsewhere, hold one element, or hold doubles.
show(Reduce(`-`, 10:1))
show(Reduce(`+`, -3:3))
show(Reduce(`+`, 5:5))
show(Reduce(`+`, 5:5, accumulate = TRUE))
show(Reduce(`+`, 5:5, 1L))
show(Reduce(`+`, 1.5:4.5))
show(Reduce(`*`, 1:20))
show(Reduce(`*`, seq_len(12)))
show(Reduce(`+`, seq_len(0)))
show(Reduce(`+`, seq_len(0), 7))
show(Reduce(`+`, seq_len(0), accumulate = TRUE))
show(Reduce(`+`, seq_along(c("a", "b", "c"))))
show(Reduce(`+`, 2147483646:2147483647 - 2147483640L))
show(tryCatch(Reduce(`+`, 2147483646:2147483647), warning = function(w) conditionMessage(w)))

# The elements a function sees are integers, one at a time, and a kept element does not change.
seen <- list()
invisible(Reduce(function(a, b) { seen[[length(seen) + 1L]] <<- b; a }, 4:7))
show(seen)
show(Reduce(function(a, b) { stopifnot(is.integer(b), length(b) == 1L); max(a, b) }, 1:9))
kept <- NULL
invisible(Reduce(function(a, b) { if (b == 2L) kept <<- b; a + b }, 1:5))
show(kept)

# The sequence is unchanged, a named sequence drops its names from the elements, and a
# sequence with attributes or a class takes the same route as any such vector.
s <- 1:6
total <- Reduce(`+`, s)
show(s); show(total)
named <- 1:3; names(named) <- c("a", "b", "c")
show(Reduce(function(a, b) c(a, b), named))
show(Reduce(function(a, b) c(a, b), named, accumulate = TRUE))
m <- 1:6; dim(m) <- 2:3
show(Reduce(`+`, m))
f <- factor(c("u", "v", "u"))
show(tryCatch(Reduce(function(a, b) paste(a, b), f), error = function(e) conditionMessage(e)))
show(Reduce(function(a, b) paste(a, b), as.character(1:4)))
show(Reduce(`+`, rev(1:5)))
show(Reduce(`+`, (1:5)[-1]))
show(Reduce(`+`, sort(c(3L, 1L, 2L))))

# Errors and early exits leave through the function.
show(tryCatch(Reduce(function(a, b) if (b == 3L) stop("at three") else a + b, 1:5),
              error = function(e) conditionMessage(e)))
first_over <- function(limit) {
  Reduce(function(a, b) if (a + b > limit) return(a) else a + b, 1:100)
}
show(first_over(20L))
show(withCallingHandlers(Reduce(function(a, b) { if (b == 2L) warning("w"); a + b }, 1:3),
                         warning = function(w) { cat("caught", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }))

# Inside a loop and a function, where the calls are compiled.
sums <- integer(0)
for (n in 1:6) sums[n] <- Reduce(`+`, 1:n)
show(sums)
triangle <- function(n) Reduce(function(a, b) a + b, seq_len(n), accumulate = TRUE)
show(triangle(8L))
show(vapply(1:5, function(n) Reduce(`*`, 1:n), 1))
