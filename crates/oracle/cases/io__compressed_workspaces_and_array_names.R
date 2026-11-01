f <- tempfile(fileext = ".rda")
a <- 1:3
for (compress in c("gzip", "bzip2", "xz")) {
  save(a, file = f, compress = compress)
  e <- new.env(); load(f, envir = e); print(c(compress, identical(e$a, 1:3)))
}
x <- 1; save(x, file = f, envir = new.env()); e <- new.env(); load(f, e); print(e$x)
save(letters, file = f); load(f)
m <- matrix(1:4, 2); names(m) <- letters[1:4]; print(names(attributes(m == 1)))
print(tryCatch(matrix(1:4, 2) > 1:8, error = conditionMessage))
print(tryCatch(read.dcf(textConnection(":x")), error = conditionMessage))
print(tryCatch(read.dcf(textConnection(" cont")), error = conditionMessage))
