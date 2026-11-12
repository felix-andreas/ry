# A start whose environment the native start does not cover runs base R's profile and the
# packages' load hooks as R code. Every environment below but the last forces that path, and the
# child prints what the profile and the hooks set. The search path leaves out the packages ry
# attaches for its own language. The paper size shows two rules of `R_HOME/etc/Renviron`: a line
# replaces an inherited value, so `R_PAPERSIZE_USER=${R_PAPERSIZE}` overrides `letter` when
# `R_PAPERSIZE` is set, and a line whose value expands to nothing assigns nothing.
root <- tempfile("start")
dir.create(file.path(root, "lib"), recursive = TRUE)
dir.create(file.path(root, "user", "4.6"), recursive = TRUE)
root <- normalizePath(root)
script <- tempfile(fileext = ".R")
writeLines(c(
  'cat("search:", setdiff(search(), c("tools:ry", "package:std", "package:ry")), "\\n")',
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
runs <- list(
  c("R_DEFAULT_PACKAGES=NULL", "R_PAPERSIZE=a4"), c("R_DEFAULT_INTERNET_TIMEOUT=17", "R_PAPERSIZE=a4"),
  c("R_REPOSITORIES=NULL", "R_PAPERSIZE=a4"), c("R_REPOSITORIES=NULL", "R_PAPERSIZE="),
  # Nothing here forces the R code, so on ry this run takes the native start.
  c("R_PAPERSIZE=")
)
for (extra in runs) {
  cat("--", extra, "\n")
  output <- system2(rscript, c("--vanilla", script), stdout = TRUE, stderr = TRUE, env = c(common, extra))
  cat(gsub(root, "ROOT", output, fixed = TRUE), sep = "\n")
}
