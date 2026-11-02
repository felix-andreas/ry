# dir.create() makes each parent in turn, so a path ending in "/." makes its directory and then
# reports that "." exists. testthat's snapshot tests build their directories this way.
root <- tempfile()
print(dir.create(file.path(root, "."), showWarnings = FALSE, recursive = TRUE))
print(dir.exists(root))
quiet <- function(expr) withCallingHandlers(expr, warning = function(w) {
  cat("warning:", sub(root, "ROOT", conditionMessage(w), fixed = TRUE), "\n")
  invokeRestart("muffleWarning")
})
print(quiet(dir.create(root)))
print(quiet(dir.create(paste0(root, "/"))))
print(quiet(dir.create(file.path(root, "a", "b"))))
print(dir.create(file.path(root, "a", "b"), recursive = TRUE))
invisible(file.create(file.path(root, c("x.md", "a/y.md"))))
print(dir(root, recursive = TRUE))

# list.dirs() names the directory itself only in a recursive listing.
print(list.dirs(root, recursive = FALSE, full.names = FALSE))
print(list.dirs(root, recursive = TRUE, full.names = FALSE))
print(length(list.dirs(file.path(root, "missing"))))

# attributes<- turns NULL into an empty list; attr<- refuses it.
x <- NULL
attributes(x) <- list(a = 1)
str(x)
y <- NULL
attributes(y) <- NULL
print(y)
print(tryCatch({ z <- NULL; attr(z, "b") <- 2 }, error = conditionMessage))

# range() flattens lists, as range.default's c(..., recursive = TRUE) does.
print(range(list(1L, 5L, 2L)))
print(range(list(1, "a")))
print(tryCatch(max(list(1, 3)), error = conditionMessage))

# file("") is an anonymous file that only opens for writing and reading.
connection <- withCallingHandlers(
  file("", open = "r"),
  warning = function(w) { print(conditionMessage(w)); invokeRestart("muffleWarning") }
)
writeLines("kept", connection)
seek(connection, 0)
print(readLines(connection))
close(connection)

# grepRaw() accepts POSIX classes in byte patterns.
print(grepRaw("^(ssh|ecdsa)-[a-z0-9-]+\\s+", charToRaw("ssh-rsa AAAA"), ignore.case = TRUE))
print(grepRaw("[[:alpha:]]+", charToRaw("12abc3"), value = TRUE))
