# A matrix that adds no line still names the other dimension, so a zero-row template supplies
# the column names of an rbind, as grDevices' table of palettes relies on.
template <- structure(numeric(0L), dim = c(0L, 2L), dimnames = list(NULL, c("a", "b")))
print(rbind(template, first = c(1, 2), second = c(3, 4)))
print(rownames(cbind(t(template), z = 1:2)))
invisible(NULL)
