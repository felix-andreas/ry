# `seq_len` over the counts a call usually passes, and the ones it coerces, warns about or refuses.

# Integer and double counts.
print(seq_len(0L))
print(seq_len(1L))
print(seq_len(2L))
print(seq_len(5L))
print(seq_len(0))
print(seq_len(1))
print(seq_len(4))
print(seq_len(2.7))
print(seq_len(0.5))
print(typeof(seq_len(3)))
print(typeof(seq_len(0L)))
print(length(seq_len(100000L)))
print(sum(as.numeric(seq_len(100000))))
print(identical(seq_len(3L), 1:3))
print(identical(seq_len(1L), 1L))
print(identical(seq_len(0L), integer(0)))

# A named argument, and the count in a variable.
print(seq_len(length.out = 3))
count <- 4L
print(seq_len(count))
count <- count + 1L
print(seq_len(count))

# The result is a value of its own.
first <- seq_len(3L)
second <- seq_len(3L)
first[2] <- 9L
print(first)
print(second)
one <- seq_len(1L)
one[1] <- 7L
print(one)
print(seq_len(1L))

# A hot loop, as `for (i in seq_len(n))` runs it.
total <- 0L
for (n in 0:300) for (i in seq_len(n)) total <- total + i
print(total)
count_up <- function(n) { total <- 0; for (i in seq_len(n)) total <- total + i; total }
for (round in 1:2000) result <- count_up(3)
print(result)
lengths <- vapply(0:6, function(n) length(seq_len(n)), integer(1))
print(lengths)

# Counts with attributes, other types and other lengths.
print(seq_len(c(a = 3L)))
print(seq_len(structure(2L, class = "count")))
print(seq_len(structure(3, note = "x")))
print(seq_len(TRUE))
print(seq_len("3"))
print(seq_len(matrix(2L, 1, 1)))
print(tryCatch(seq_len(c(2L, 5L)), warning = function(w) conditionMessage(w)))
print(tryCatch(seq_len(integer(0)), error = function(e) conditionMessage(e), warning = function(w) conditionMessage(w)))

# Counts that are refused.
print(tryCatch(seq_len(-1L), error = function(e) conditionMessage(e)))
print(tryCatch(seq_len(-0.5), error = function(e) conditionMessage(e)))
print(tryCatch(seq_len(NA_integer_), error = function(e) conditionMessage(e)))
print(tryCatch(seq_len(NA_real_), error = function(e) conditionMessage(e)))
print(tryCatch(seq_len(NaN), error = function(e) conditionMessage(e)))
print(tryCatch(seq_len(Inf), error = function(e) conditionMessage(e)))
print(tryCatch(seq_len(NULL), error = function(e) conditionMessage(e), warning = function(w) conditionMessage(w)))
print(tryCatch(seq_len("a"), error = function(e) conditionMessage(e), warning = function(w) conditionMessage(w)))
print(tryCatch(seq_len(), error = function(e) "error"))

# A count past the integer range is a sequence of doubles.
big <- seq_len(2^31)
print(typeof(big))
print(length(big))
print(big[2^31])
edge <- seq_len(2^31 - 1)
print(typeof(edge))
print(length(edge))
