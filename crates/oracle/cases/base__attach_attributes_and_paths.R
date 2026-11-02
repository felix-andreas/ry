# attach() copies an environment's bindings, leaving the environment and its parent alone.
e <- new.env()
assign("z", 1, e)
attach(e, name = "copied")
assign("z", 2, e)
get("z", as.environment("copied"))
identical(as.environment("copied"), e)
detach("copied")

# attributes<- on NULL gives an empty list; an empty name is an error, an NA name is not.
x <- NULL
attributes(x) <- list(a = 1)
x
y <- 1
tryCatch(attributes(y) <- list(y = 1, 2), error = conditionMessage)
z <- 1
attributes(z) <- list(a = 1, b = 2)[c("a", "missing")]
attributes(z)

# Compact row names written as the double c(NA, n) are stored as integers.
d <- structure(list(a = 1:3), class = "data.frame", row.names = c(NA, 3))
typeof(attr(d, "row.names"))
.row_names_info(d)
dim(d)

# A logical subscript longer than the vector extends it, even past its last TRUE.
v <- NA
v[c(TRUE, FALSE, TRUE, FALSE)] <- 1:2
v
w <- 1:2
w[c(FALSE, FALSE, FALSE, FALSE)] <- 5L
w
rank(c(2, NA, 4, NaN, 4, 2, NA), ties.method = "min", na.last = "keep")

# range() flattens lists as c(..., recursive = TRUE) does.
range(list(1L, 2L, 3L))
range(list(1, list(5, -2)))

# dirname() as GNU R's R_dirname: one trailing separator goes, an empty path stays empty.
dirname(c("", "/", "a//b", "a/b//", "//", "a", NA))

# enc2utf8() translates latin1 strings.
s <- "fa\xE7ile"
Encoding(s) <- "latin1"
Encoding(enc2utf8(s))
enc2utf8(s) == "fa\u00e7ile"

# A file() connection that sniffed gzip data decompresses in binary mode too.
path <- tempfile()
saveRDS(1:3, path)
con <- file(path)
open(con, "rb")
bytes <- readBin(con, raw(), 1000)
close(con)
identical(unserialize(bytes), 1:3)
