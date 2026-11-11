# A for loop binds its variable to NULL before the first iteration, so the variable exists after
# a loop over an empty sequence, and a value it held before is replaced.
f <- function() {
  for (k in integer()) {}
  list(exists("k", inherits = FALSE), k)
}
str(f())

g <- function() {
  k <- 5
  for (k in NULL) {}
  k
}
print(g())

h <- function(xs) {
  for (k in xs) {}
  k
}
print(h(list()))
print(h(character()))
print(h(1:3))
print(h(list("a", 2)))

# A hot function, so the compiled tiers see the empty loop too.
count <- 0L
for (i in 1:2000) {
  if (is.null(h(if (i %% 2 == 0) integer() else 1:4))) count <- count + 1L
}
print(count)

for (top in character()) {}
print(top)

e <- new.env()
local(for (inner in double()) {}, envir = e)
print(exists("inner", envir = e, inherits = FALSE))
print(e$inner)
