x <- c(a = 1, b = 2, c = 3, d = 4)
x[c(TRUE, FALSE)]; x[-1]; x[c("b", "d")]; x[x > 2]; x[10]; x[0]
x[["c"]]
l <- list(p = 1, q = "two", r = list(s = 3))
l$q; l[["r"]][["s"]]; l["p"]; l[c("p", "q")]
l$nonexistent
m <- matrix(1:12, 3, dimnames = list(letters[1:3], LETTERS[1:4]))
m[2, ]; m[, "C"]; m[2, 3]; m[m > 10]; m[, 2, drop = FALSE]; m[-1, c("A", "D")]
which(c(FALSE, TRUE, TRUE)); which.max(c(3, 9, 2)); which(m > 6, arr.ind = FALSE)
v <- 1:10; v[v %% 2 == 0] <- 0L; v
head(letters, 3); tail(1:10, 2)
tryCatch(list(1)[[3]], error = function(e) conditionMessage(e))
match(c("b", "z"), letters); c("b", "z") %in% letters
