# A function lookup passes bindings that hold non-functions. Giving such a binding a function
# value must change what the lookup finds, including in code that has already been compiled.
c <- 1
g <- function(x) c(x, 2)
for (i in 1:5) print(g(i))
c <- function(...) "mine"
print(g(1))
rm(c)
print(g(1))

# The same through a function's enclosure rather than the global environment.
make <- function() {
  sum <- "not a function"
  function(x) sum(x, 1)
}
h <- make()
for (i in 1:5) print(h(i))
assign("sum", function(...) "local", envir = environment(h))
print(h(1))
