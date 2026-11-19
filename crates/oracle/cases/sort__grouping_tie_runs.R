# grouping() and radixsort(retgrp = TRUE) report the group ends that the radix sort's tie runs
# give: dense integer keys, wide doubles through the digit passes, several keys with short and
# long runs, decreasing keys and NAs.
show <- function(g) {
  cat(length(g), sum(as.numeric(g) * seq_along(g)), "\n")
  ends <- attr(g, "ends")
  cat(length(ends), sum(as.numeric(ends)), attr(g, "maxgrpn"), class(g), "\n")
}
set.seed(11)
show(grouping(sample.int(50L, 1e5, TRUE)))
show(grouping(sample(c(runif(3000), NA, 0, -0), 1e5, TRUE)))
show(grouping(sample.int(3e5, 2e5, TRUE)))
a <- sample.int(4L, 2e4, TRUE); b <- sample(c(1.5, 2.5, NA), 2e4, TRUE)
show(grouping(a, b))
c <- sample.int(5000L, 2e4, TRUE); d <- sample.int(3L, 2e4, TRUE)
show(grouping(c, d))
show(grouping(c, d, sample(letters[1:2], 2e4, TRUE)))
g <- .Internal(radixsort(TRUE, c(TRUE, FALSE), TRUE, TRUE, a, d))
show(g)
g <- .Internal(radixsort(NA, FALSE, TRUE, TRUE, c(3L, NA, 1L, 3L, NA, 1L)))
print(unclass(g))
print(unclass(grouping(1:5)))
print(unclass(grouping(rep(7, 6))))
print(unclass(grouping(c(2L, 1L, 2L, 1L), c(1, 1, 1, 2))))
# Strings group by first appearance within each run, except that a run strictly in the
# opposite byte order (NA last under na.last = TRUE) reverses.
p <- function(...) print(as.vector(grouping(...)))
p(c("b", "a")); p(c("c", "b", "a")); p(c("c", "b", "b")); p(c("b", NA, "a")); p(c(NA, "b", "a"))
p(c("a", "B")); p(c(1L, 1L), c("b", "a"))
set.seed(10)
p(sample.int(2L, 10, TRUE), sample.int(3L, 10, TRUE), sample(letters[1:2], 10, TRUE))
p(sample.int(40L, 400, TRUE), sample(c(letters[1:3], NA), 400, TRUE))
p(sample(c("x", "y", NA), 300, TRUE), sample.int(4L, 300, TRUE), sample(c("b", "a"), 300, TRUE))
radix <- function(na.last, decreasing, ...)
  print(as.vector(.Internal(radixsort(na.last, decreasing, FALSE, FALSE, ...))))
radix(TRUE, TRUE, c("b", "a")); radix(TRUE, TRUE, c("a", "b")); radix(TRUE, TRUE, c("a", "b", "a"))
radix(FALSE, FALSE, c("b", NA, "a")); radix(FALSE, FALSE, c(NA, "b", "a")); radix(FALSE, FALSE, c("b", "a", NA))

# With na.last = NA, a run of strings in reverse byte order that holds an NA is not reversed.
print(.Internal(radixsort(NA, FALSE, FALSE, FALSE, c("c", "b", NA))))
print(.Internal(radixsort(NA, FALSE, FALSE, FALSE, c("c", "b", "a"))))
print(.Internal(radixsort(TRUE, FALSE, FALSE, FALSE, c("c", "b", NA))))
# Strings kept in order of appearance drop no NA from the first key unless all of it is NA,
# and a later key drops its NA rows.
appearance <- function(...) {
  print(.Internal(radixsort(NA, rep(FALSE, ...length()), FALSE, FALSE, ...)))
}
appearance(c(NA_character_, NA))
appearance(c(NA_character_, NA), 2:1)
appearance(c("a", NA, "b"), 1:3)
appearance(1:3, c("a", NA, "b"))
appearance(c("b", NA, "a"))
appearance(c("a", NA), c(NA_character_, NA))
appearance(c("a", "b"), c(NA_character_, NA))
