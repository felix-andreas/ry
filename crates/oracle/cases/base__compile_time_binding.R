# Compiled code binds the base functions GNU R's compiler inlines when it compiles, so a function
# that masks one of them later does not reach that code. Names it does not inline, code it binds
# after a mask, and code GNU R does not compile at all look the function up as they run.
minus <- function(a, b) base::`-`(a, b)

# A top-level loop compiles when it starts, so a mask attached in the middle does not show.
total <- 0
for (i in 1:40) {
  if (i == 10) attach(list(`+` = minus), name = "masking_plus")
  total <- total + 1
}
print(total)
detach("masking_plus")

# A global function with a loop compiles at its first call and keeps what it bound.
count_up <- function(n) { s <- 0; for (i in seq_len(n)) s <- s + 1; s }
print(count_up(3))
attach(list(`+` = minus), name = "late_plus")
print(count_up(3))
detach("late_plus")

# A mask attached before compiling is what the code calls, and it looks `+` up as it runs.
attach(list(`+` = minus), name = "early_plus")
count_early <- function(n) { t <- 0; for (j in seq_len(n)) t <- t + 1; t }
print(count_early(3))
detach("early_plus")
print(count_early(3))
total <- 0
attach(list(`+` = minus), name = "early_loop_plus")
for (i in 1:5) total <- total + 1
detach("early_loop_plus")
print(total)

# A function's own definition of `+`, before or after the loop, keeps its calls dynamic.
local_plus <- function(n) {
  s <- 0
  for (i in seq_len(n)) s <- s + 1
  `+` <- minus
  s <- s + 1
  s
}
print(local_plus(3))
outer_plus <- function(n) {
  `+` <- minus
  inner <- function() { s <- 0; for (i in seq_len(n)) s <- s + 1; s }
  inner()
}
print(outer_plus(3))

# `sqrt` has no inline handler at the default level outside a namespace, so a mask shows at once.
total <- 0
for (i in 1:40) {
  if (i == 10) attach(list(sqrt = function(x) 100), name = "masking_sqrt")
  total <- total + sqrt(1)
}
print(total)
detach("masking_sqrt")
root_of <- function(x) { for (i in 1:2) NULL; sqrt(x) }
print(root_of(4))
attach(list(sqrt = function(x) 100), name = "late_sqrt")
print(root_of(4))
detach("late_sqrt")

# A small function made inside another is never compiled by GNU R, so it sees a later mask.
make_adder <- function() function(x) x + 1
add_one <- make_adder()
print(c(add_one(1), add_one(1), add_one(1)))
attach(list(`+` = minus), name = "uncompiled_plus")
print(add_one(1))
detach("uncompiled_plus")
