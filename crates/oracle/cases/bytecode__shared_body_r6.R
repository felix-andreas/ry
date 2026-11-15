# oracle: requires R6
# R6 copies every method into each new object with `environment<-`, so all objects of a class,
# and of every class built from the same function, share one body. A non-portable class keeps
# its fields in the method's enclosure, where a field named `length` shadows base's function
# for that class only.
method <- function() length(items) + sum(items)
init <- function(items) items <<- items
Shadowing <- R6::R6Class("Shadowing",
  portable = FALSE,
  public = list(items = NULL, length = function(x) 1000L, total = method, initialize = init)
)
Plain <- R6::R6Class("Plain",
  portable = FALSE,
  public = list(items = NULL, total = method, initialize = init)
)
for (round in 1:4) {
  shadowing <- Shadowing$new(1:round)
  plain <- Plain$new(1:round)
  print(c(shadowing$total(), plain$total(), shadowing$total(), plain$total()))
}

# Many short-lived objects of one portable class, with collections between them.
Stack <- R6::R6Class("Stack", public = list(
  items = list(),
  push = function(x) {
    self$items[[length(self$items) + 1L]] <- x
    invisible(self)
  },
  size = function() length(self$items)
))
sizes <- integer(0)
for (k in 1:24) {
  stack <- Stack$new()
  for (j in seq_len(k %% 5)) stack$push(j)
  sizes[k] <- stack$size()
  if (k %% 8 == 0) invisible(gc())
}
print(sizes)
