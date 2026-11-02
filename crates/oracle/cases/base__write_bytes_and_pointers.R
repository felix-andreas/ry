# writeLines(useBytes = TRUE) writes latin1 text as its bytes; without it, as UTF-8.
y <- iconv("août\nél", "UTF-8", "latin1")
f <- tempfile()
con <- file(f, "wb"); writeLines(y, con, useBytes = TRUE); close(con)
print(readBin(f, "raw", 20))
con <- file(f, "wb"); writeLines(y, con); close(con)
print(readBin(f, "raw", 20))

# A null external pointer prints as C's %p does.
p <- new("externalptr")
print(p)
deparse(p)

# Double compact row names are stored as integers.
d <- data.frame(a = 1:3)
attr(d, "row.names") <- c(NA_real_, -3)
str(attributes(d)$row.names)
.row_names_info(d)
