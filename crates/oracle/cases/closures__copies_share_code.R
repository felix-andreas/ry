# A copy of a closure (`environment<-`, a replacement call that duplicates it first, R6-style
# copies into fresh environments) behaves as the original, whichever of them runs first and
# however often each runs, while each copy reads its own enclosure.
make <- function() function(x) x * scale + offset
f <- make()
copies <- lapply(1:4, function(i) {
  g <- f
  environment(g) <- list2env(list(scale = i, offset = 10 * i))
  g
})
for (k in 1:3) print(vapply(copies, function(g) g(2), numeric(1)))
scale <- 100; offset <- 0
environment(f) <- globalenv()
print(f(2))
h <- f
attr(h, "tag") <- "copy"
print(c(f(1), h(1)), digits = 3)
print(identical(body(f), body(h)))
methods <- list(push = function(v) { items <<- c(items, v); length(items) },
                size = function() length(items))
objects <- lapply(1:3, function(i) {
  env <- new.env()
  env$items <- seq_len(i)
  lapply(methods, function(m) { environment(m) <- env; m })
})
for (k in 1:2) print(vapply(objects, function(o) o$push(k) + o$size(), numeric(1)))
