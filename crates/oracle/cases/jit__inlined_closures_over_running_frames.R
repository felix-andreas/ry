# A closure the optimizing tier inlines can be enclosed by the environment of a function that is
# still running. That environment binds only the variables a lookup has asked for, and the
# running function keeps the others, so code that finds a free variable by reading the
# enclosing environments' own tables has to ask the running function for them.

step <- 10
session <- function(define, n) {
  base <- 2
  if (define) step <- 1
  advance <- function(x) x + step
  peek <- function() base
  drive <- function(n) {
    total <- 0
    for (i in seq_len(n)) total <- total + advance(i)
    total
  }
  peek()
  drive(n)
}
for (k in 1:60) session(FALSE, 10L)
print(session(FALSE, 10L))
print(session(TRUE, 10L))
print(session(FALSE, 10L))
print(session(TRUE, 3000L))

# The variable is read for the first time after the loop was compiled.
within <- function(n) {
  step <- 1
  base <- 2
  advance <- function(x, late) if (late) x + step else x + base
  total <- 0
  for (i in seq_len(n)) total <- total + advance(i, i > n - 3L)
  total
}
print(within(3000L))
print(within(3000L))

nested <- function(n) {
  step <- 1
  base <- 2
  advance <- function(x, late) if (late) x + step else x + base
  drive <- function(n) {
    total <- 0
    for (i in seq_len(n)) total <- total + advance(i, i > n - 3L)
    total
  }
  drive(n)
}
print(nested(3000L))
print(nested(3000L))

# The running function assigns the variable after its closure read the global of that name.
rebind <- function(n) {
  advance <- function(x) x + step
  drive <- function(n) {
    total <- 0
    for (i in seq_len(n)) total <- total + advance(i)
    total
  }
  before <- drive(n)
  step <- 1
  c(before, drive(n))
}
print(rebind(3000L))
print(rebind(3000L))

# One call of the enclosing function compiles its closure, and the compiled code then reads a
# local of that function that nothing had read before.
rounds <- function(count, n) {
  step <- 1
  base <- 2
  advance <- function(x, late) if (late) x + step else x + base
  drive <- function(n, late) {
    total <- 0
    for (i in seq_len(n)) total <- total + advance(i, late)
    total
  }
  out <- 0
  for (r in seq_len(count)) out <- out + drive(n, FALSE)
  c(out, drive(n, TRUE))
}
print(rounds(200L, 100L))
print(rounds(200L, 3000L))

after_loop <- function(n) {
  step <- 1
  base <- 2
  drive <- function(n) {
    total <- 0
    for (i in seq_len(n)) total <- total + base
    total + step
  }
  drive(n)
}
print(after_loop(100000L))
print(after_loop(100000L))
