# An array with no slices names its shape, then prints the labels of its first two dimensions
# with no cells. A zero first or second extent still prints each slice.
print(array(integer(0), c(2, 3, 0)))
print(array(numeric(0), c(0, 3, 2)))
print(array(character(0), c(2, 0, 2)))
print(array(integer(0), c(2, 3, 0, 2)))
print(array(integer(0), c(2, 0, 2), dimnames = list(c("a", "b"), NULL, c("p", "q"))))
print(array(integer(0), c(2, 3, 0), dimnames = list(A = c("a", "b"), B = c("u", "v", "w"), C = NULL)))
print(array(list(), c(2, 3, 0)))
print(array(logical(0), c(0, 0, 0)))
print(array(integer(0), c(0, 3, 0)))
print(array(integer(0), c(2, 0, 0)))
print(array(complex(0), c(1, 1, 0)))
print(array(character(0), c(1, 2, 0)), quote = FALSE)
