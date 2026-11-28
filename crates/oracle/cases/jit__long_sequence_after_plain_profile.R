# A closure that first saw plain vectors compiles with that profile and then gets compact
# sequences of up to 2^31 - 1 elements. Its compiled code must not copy such a sequence into a
# plain vector, which would take 8 GB (16 GB for doubles) that R never allocates: it leaves for
# tier 0 once, and the recompile keeps the variable boxed for the calls after it. A variable the
# body assigns into is expanded by R anyway, so its load copies the sequence. `typeof` is base
# R's closure `function(x) .Internal(typeof(x))`, and `seq_len(0L)` gives a plain vector.
first <- typeof(seq_len(0L))
second <- typeof(seq_len(.Machine$integer.max))
print(c(first, second))
print(typeof(seq_len(5L)))
last <- function(x) x[length(x)]
print(last(integer(0)))
print(last(c(1L, 2L)))
for (k in 0:3) print(last(seq_len(.Machine$integer.max - k)))
last_double <- function(x) x[length(x)]
print(last_double(c(0.5, 1.5)))
print(last_double(c(2.5, 3.5)))
for (k in 0:2) print(last_double(as.double(seq_len(.Machine$integer.max - k))))
through <- function(x) last(x) + 0L
print(through(c(3L, 4L)))
print(through(c(5L, 6L)))
for (k in 0:2) print(through(seq_len(.Machine$integer.max - k)))
bumped <- function(x) {
  x[1L] <- 0L
  x[length(x)]
}
print(bumped(c(1L, 2L)))
print(bumped(c(3L, 4L)))
for (k in 1:3) print(bumped(seq_len(2^18 + k)))
for (k in 1:2) print(typeof(seq_len(3L)))
