# `Sys.which` finds each name's program on `PATH` the way `which` does, in a directory the case
# makes so the paths are the same on every machine: executable files only, a name with a slash
# taken as it is, and an empty `PATH` entry standing for the working directory.
directory <- normalizePath(tempfile("which"), mustWork = FALSE)
dir.create(directory)
tool <- file.path(directory, "tool")
data <- file.path(directory, "data")
writeLines("#!/bin/sh", tool)
writeLines("plain", data)
Sys.chmod(tool, "755")
Sys.chmod(data, "644")
show <- function(found) {
    names(found) <- gsub(directory, "DIR", names(found), fixed = TRUE)
    print(gsub(directory, "DIR", found, fixed = TRUE))
}
old <- Sys.getenv("PATH")
Sys.setenv(PATH = paste(directory, old, sep = ":"))
show(Sys.which(c("tool", "data", "absent-program", NA, "")))
show(Sys.which(c(tool, data)))
show(Sys.which(character()))
old_directory <- setwd(directory)
Sys.setenv(PATH = paste0(":", old))
show(Sys.which("tool"))
setwd(old_directory)
Sys.setenv(PATH = old)
unlink(directory, recursive = TRUE)
