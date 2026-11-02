# A matrix with no columns prints its row labels alone, and named dimnames add the column title
# line above them, even when that title is empty.
matrix(numeric(0), 2, 0)
matrix(numeric(0), 2, 0, dimnames = list(r = c("a", "bb"), c = NULL))
matrix(numeric(0), 2, 0, dimnames = list(r = c("a", "bb"), NULL))
matrix(numeric(0), 2, 0, dimnames = list(NULL, c = NULL))
matrix(numeric(0), 2, 0, dimnames = list(longname = c("a", "bb"), c = NULL))
matrix(numeric(0), 2, 0, dimnames = list(r = c("a", "bb"), longname = NULL))
print(matrix(character(0), 2, 0, dimnames = list(r = c("a", "bb"), cc = NULL)), quote = FALSE)
matrix(numeric(0), 0, 0, dimnames = list(r = character(0), cc = character(0)))
matrix(numeric(0), 0, 3, dimnames = list(rows = NULL, cols = c("a", "bb", "ccc")))
