# A start whose environment the native start does not cover runs base R's profile and the
# packages' load hooks as R code. Each environment below forces that path, and the child prints
# what the profile and the hooks set.
root <- tempfile("start")
dir.create(file.path(root, "lib"), recursive = TRUE)
dir.create(file.path(root, "user", "4.6"), recursive = TRUE)
root <- normalizePath(root)
script <- tempfile(fileext = ".R")
writeLines(c(
  'cat("search:", search(), "\\n")',
  'cat("lib paths:", .libPaths()[1:2], "\\n")',
  'cat("R_LIBS_USER:", Sys.getenv("R_LIBS_USER"), "\\n")',
  'cat("timeout:", getOption("timeout"), "\\n")',
  'cat("papersize:", getOption("papersize"), "\\n")',
  'cat("editor:", getOption("editor"), "\\n")',
  'cat("utils loaded:", isNamespaceLoaded("utils"), "\\n")',
  'cat("locale:", Sys.getlocale("LC_CTYPE"), "\\n")'
), script)
rscript <- file.path(R.home("bin"), "Rscript")
common <- c(
  "LANG=C", "LC_ALL=C", "R_PAPERSIZE_USER=letter", "EDITOR=ed-for-test",
  paste0("R_LIBS=", file.path(root, "lib")), paste0("R_LIBS_USER=", file.path(root, "user", "%v"))
)
for (extra in list("R_DEFAULT_PACKAGES=NULL", "R_DEFAULT_INTERNET_TIMEOUT=17", "R_REPOSITORIES=NULL")) {
  cat("--", extra, "\n")
  output <- system2(rscript, c("--vanilla", script), stdout = TRUE, stderr = TRUE, env = c(common, extra))
  cat(gsub(root, "ROOT", output, fixed = TRUE), sep = "\n")
}
