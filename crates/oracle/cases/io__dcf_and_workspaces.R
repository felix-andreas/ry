dcf <- tempfile(fileext = ".dcf")
writeLines(c(
  "Package: a", "Description: first line", "   second   line", "\tthird",
  "Title:  spaced  ", "A: 1", "A: 2", "B:", "  cont", " .", "  more",
  "", "", "Package: b", "Extra: x", "  y"
), dcf)
print(read.dcf(dcf))
print(read.dcf(dcf, keep.white = c("Title", "B", "Extra")))
print(read.dcf(dcf, fields = c("Package", "Nope")))
print(dim(read.dcf(textConnection(character()))))
bad <- tempfile()
writeLines("not a field", bad)
print(tryCatch(read.dcf(bad), error = function(e) "malformed"))

con <- file(dcf, "rb")
print(readChar(con, 8L, useBytes = TRUE))
print(readChar(con, 3L, useBytes = TRUE))
close(con)

workspace <- tempfile(fileext = ".rda")
answer <- 42L
label <- c(x = "a", y = "b")
save(answer, label, file = workspace)
target <- new.env()
print(load(workspace, envir = target))
print(sort(ls(target)))
print(target$answer)
print(target$label)
