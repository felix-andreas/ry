# rep() fills by doubling a copied prefix: every source length, including ones that do not
# divide the result, and every vector type.
for (source in list(0.5, c(1, 2), c(1L, 2L, 3L), c(TRUE, NA, FALSE), c("a", "b"),
                    list(1, "x"), as.raw(1:3), c(1+2i, 3-1i))) {
  for (total in c(0L, 1L, 2L, 5L, 7L, 33L)) {
    print(rep_len(source, total))
  }
  print(identical(rep(source, 17L), rep_len(source, 17L * length(source))))
  print(identical(rep.int(source, 5L), rep(source, times = 5L)))
}
x <- rep(c(a = 1, b = 2), 3L)
print(x)

# is.unsorted() on plain doubles, integers and logicals, with NA and NaN, strictly or not,
# across the 16-element blocks of the order check.
check <- function(x) {
  cat(sprintf("%-3d %-5s %-5s %-5s %-5s\n", length(x), is.unsorted(x), is.unsorted(x, strictly = TRUE),
              .Internal(is.unsorted(x, FALSE, FALSE)), .Internal(is.unsorted(x, FALSE, TRUE))))
}
for (n in c(0L, 1L, 2L, 15L, 16L, 17L, 33L, 100L)) {
  ascending <- seq_len(n) / 2
  check(ascending)
  check(as.integer(ascending * 2))
  check(rev(ascending))
  if (n > 1L) {
    tie <- ascending
    tie[n] <- tie[n - 1L]
    check(tie)
    check(as.integer(tie * 2))
    late <- ascending
    late[n] <- -1
    check(late)
    gap <- ascending
    gap[n %/% 2L + 1L] <- NaN
    print(.Internal(is.unsorted(gap, FALSE, FALSE)))
    print(.Internal(is.unsorted(gap, FALSE, TRUE)))
    holes <- as.integer(ascending * 2)
    holes[n %/% 2L + 1L] <- NA
    print(.Internal(is.unsorted(holes, FALSE, FALSE)))
    print(.Internal(is.unsorted(holes, FALSE, TRUE)))
  }
}
check(c(FALSE, FALSE, TRUE, TRUE))
check(c(-0, 0, 0))

# order() and sort() on input already in order, or strictly in reverse.
for (n in c(1L, 16L, 17L, 40L)) {
  up <- seq_len(n) + 0.5
  print(order(up))
  print(order(rev(up)))
  print(order(up, decreasing = TRUE))
  ties <- c(up, up[n])
  print(order(ties))
  print(order(rev(ties)))
  print(sort(rev(ties)))
}
print(order(c(1, NaN, 2)))
print(order(c(1, NA, 2), na.last = FALSE))

# unique() and duplicated() on lists: by identical(), first or last occurrence kept.
values <- list(1, 1L, "1", 1, NULL, NA, NA_real_, NaN, NA_real_, 0, -0, list(1, 2), list(1, 2),
               list(1, list(2)), c(a = 1), c(b = 1), 1:3, 1:3 + 0L, quote(x), quote(x), sum, sum)
print(duplicated(values))
print(duplicated(values, fromLast = TRUE))
str(unique(values))
print(length(unique(values, fromLast = TRUE)))
print(anyDuplicated(values))
big <- as.list(rep(c(3, 1, 2, 3.5), 300L))
print(sum(duplicated(big)))
str(unique(big))
print(which(!duplicated(big, fromLast = TRUE)))
print(duplicated(list(1, 2, 1, 2), incomparables = list(2)))
print(unique(list()))
