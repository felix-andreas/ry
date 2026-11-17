# A subscript that takes a reference to the assignment's variable leaves that reference
# unchanged: the attribute a nested assignment writes into is copied once the variable is
# shared, as GNU R copies every attribute its getter returns.
attributes_of <- function() {
  x <- c(a = 1, b = 2)
  names(x)[{ keep <- x; 2 }] <- "z"
  f <- factor(c("p", "q"))
  levels(f)[{ kept_factor <- f; 1 }] <- "z"
  m <- matrix(1:4, 2, dimnames = list(c("r", "s"), NULL))
  rownames(m)[{ kept_matrix <- m; 1 }] <- "z"
  u <- structure(1:2, u = c("i", "j"))
  attr(u, "u")[{ kept_attr <- u; 2 }] <- "z"
  list(names(keep), names(x), levels(kept_factor), levels(f), rownames(kept_matrix),
    rownames(m), attr(kept_attr, "u"), attr(u, "u"))
}

# Each shape runs several times, so the compiled tiers see it too.
for (round in 1:4) print(attributes_of())

x <- c(a = 1, b = 2)
names(x)[{ keep <- x; 2 }] <- "z"
print(names(keep))
print(names(x))

# Without a reference taken, the attribute is written in place, round after round.
renamed <- function(n) {
  x <- c(a = 1, b = 2, c = 3)
  for (i in seq_len(n)) names(x)[i] <- paste0("n", i)
  x
}
for (round in 1:3) print(renamed(3))
