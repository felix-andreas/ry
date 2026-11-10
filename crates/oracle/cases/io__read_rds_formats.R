# readRDS() of a path reads gzip, uncompressed, xz and bzip2 files and ASCII streams, and
# reports a missing file and a save() file with GNU R's warnings and errors.
value <- list(a = 1:3, b = "text", f = function(x) x + 1, e = quote(f(y)))
dir <- tempfile()
dir.create(dir)
write_and_read <- function(name, ...) {
  path <- file.path(dir, name)
  saveRDS(value, path, ...)
  back <- readRDS(path)
  stopifnot(identical(back$a, value$a), identical(back$b, value$b), identical(back$e, value$e))
  back$f(1)
}
write_and_read("gzip.rds")
write_and_read("plain.rds", compress = FALSE)
write_and_read("xz.rds", compress = "xz")
write_and_read("bzip2.rds", compress = "bzip2")
write_and_read("ascii.rds", ascii = TRUE)
write_and_read("ascii_plain.rds", ascii = TRUE, compress = FALSE)
str(readRDS(file.path(R.home("library"), "stats", "Meta", "package.rds"))$DESCRIPTION[["Package"]])
saved <- file.path(dir, "saved.RData")
x <- 1
save(x, file = saved)
tryCatch(readRDS(saved), error = function(e) conditionMessage(e))
empty <- file.path(dir, "empty.rds")
file.create(empty)
tryCatch(readRDS(empty), error = function(e) conditionMessage(e))
missing_path <- file.path(dir, "missing.rds")
withCallingHandlers(
  tryCatch(readRDS(missing_path), error = function(e) class(e)),
  warning = function(w) invokeRestart("muffleWarning")
)
unlink(dir, recursive = TRUE)
