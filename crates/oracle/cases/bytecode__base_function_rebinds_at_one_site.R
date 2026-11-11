# One call site of a base builtin, called before and after a global definition masks it, after
# the mask is removed, and while an attached package masks it. The site's fused instructions
# (the function lookup with the builtin test, the argument read with the builtin call) must see
# every change. Masking a base special is left out: GNU R's compiler inlines one when JIT
# compilation is on, so its result depends on the JIT setting.
count <- function(x) length(x)
for (i in 1:3) print(count(1:4))
length <- function(x) "masked length"
print(count(1:4))
length <- function(x) "masked again"
print(count(1:4))
rm(length)
print(count(1:4))
attach(list(length = function(x) "attached length"), name = "masks")
print(count(1:4))
detach("masks")
print(count(1:4))
