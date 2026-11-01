path <- tempfile()
writeLines(c(
  "Package: foo", "Title:   A  title  ", "Description: line one", "   line two", "  .",
  "  line three", "A: 1", "A: 2", "", "", "  ", "Package: bar", "B:", "C: tab\there", "\tcont"
), path)
.Internal(readDCF(file(path), NULL, NULL))
.Internal(readDCF(file(path), c("Description", "Zed"), "Description"))
read.dcf(path, fields = c("Package", "A"))
str(read.dcf(path, all = TRUE))
writeLines(c("Package: x", "not a field"), path)
tryCatch(read.dcf(path), error = conditionMessage)
