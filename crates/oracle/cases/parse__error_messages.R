# parse() names the token R's grammar stops on, gives its line and column, and quotes the last
# lines read with a caret under the column. testthat matches on this text.
texts <- list(
  "x <- 1\n\nbut this is", "f(1,,", "x <- )", "1 +", "1 +\n", c("1 +", ""), "'abc", "a b",
  "if x", "x[[1]", "5 5", "f(a b)", "\"a\" b", "x\t\ty z", "é è z",
  "a <- 1\n\n\n\nf(", "a = = 2", "x -> -> y", "a %in% %in% b", "NA_character_ 1", "TRUE 1",
  "NULL 1", "f(x = , y == )", "a$1", "a::1", "if (x) 1 else else", "function(x) x x",
  "{1 2}", "x <<- <- 1", "\\(x) 1 1", "`a` b", "a ] b", "x[[1]]]", "...  1", "..1 1",
  "1i 2", "0x10 1", "1L 2", c(strrep("a <- 1;", 40), "b c")
)
for (text in texts) cat(tryCatch(parse(text = text), error = conditionMessage), "\n", sep = "")

# A file names itself, and source() reports the same message.
path <- tempfile(fileext = ".R")
writeLines("x <- 1\nbut this", path)
message <- tryCatch(source(path), error = conditionMessage)
cat(sub(path, "FILE", message, fixed = TRUE), "\n", sep = "")

# Rscript quotes only the failing top-level expression.
script <- tempfile(fileext = ".R")
rscript <- file.path(R.home("bin"), "Rscript")
for (body in c("x <- 1; y <- f(1,\n  2 3)", "x <- 1\nf(\n\n  # c\n  a b)", "x <- (1 +", "print(1)\nif (x) 1 else else")) {
  writeLines(body, script)
  print(suppressWarnings(system2(rscript, c("--vanilla", script), stdout = TRUE, stderr = TRUE)))
}

# list.dirs() names the directory itself only in a recursive listing.
root <- tempfile()
dir.create(file.path(root, "windows", "inner"), recursive = TRUE)
print(list.dirs(root, recursive = FALSE, full.names = FALSE))
print(list.dirs(root, recursive = TRUE, full.names = FALSE))
print(length(list.dirs(file.path(root, "missing"))))
