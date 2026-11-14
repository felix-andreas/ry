# A recursive dir.create() succeeds on a deep path whose parents exist, and reports a file that
# stands where an ancestor directory should be as "Not a directory".
root <- tempfile()
quiet <- function(expr) withCallingHandlers(expr, warning = function(w) {
  cat("warning:", sub(root, "ROOT", conditionMessage(w), fixed = TRUE), "\n")
  invokeRestart("muffleWarning")
})
deep <- file.path(root, "a", "b", "c", "d")
print(dir.create(deep, recursive = TRUE))
print(dir.exists(deep))
print(quiet(dir.create(deep, recursive = TRUE)))
print(dir.create(file.path(deep, "e"), recursive = TRUE))
print(dir.create(file.path(root, "a", "x", "y"), recursive = TRUE))

file.create(file.path(root, "file"))
print(quiet(dir.create(file.path(root, "file", "sub", "leaf"), recursive = TRUE)))
print(quiet(dir.create(file.path(root, "file", "leaf"), recursive = TRUE)))
print(sort(list.dirs(root, full.names = FALSE)))
