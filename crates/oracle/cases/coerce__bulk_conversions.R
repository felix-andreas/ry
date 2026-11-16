# Logical to character interns TRUE and FALSE once; double to integer checks NaN and range
# before truncating; as.vector(mode = "character") of plain numbers defers the strings.
x <- c(TRUE, FALSE, NA, TRUE)
str(as.character(x))
str(as.character(logical()))
str(as.character(c(a = TRUE, b = NA)))
y <- c(1.9, -1.9, NA, NaN, Inf, -Inf, 2147483647, -2147483647, 2147483648, -2147483648, 0.5)
str(suppressWarnings(as.integer(y)))
tryCatch(as.integer(3e9), warning = function(w) conditionMessage(w))
str(as.integer(c(a = 2.5)))
str(as.double(c(1L, NA, -3L)))
str(as.double(c(TRUE, NA)))
str(as.numeric(structure(1:2, dim = 2L)))
str(as.vector(1:3, "character"))
str(as.vector(c(1.5, NA, 1e-20), "character"))
str(as.vector(c(a = 1L), "character"))
str(as.vector(structure(1:2, class = "foo"), "character"))
z <- as.vector(seq_len(5) * 2, "character")
z[2] <- "x"
z
