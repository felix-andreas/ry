# On Linux, utils' osVersion is the PRETTY_NAME of /etc/os-release without its quotes, which
# ry reads natively and GNU R reads through a shell pipeline.
if (Sys.info()[["sysname"]] == "Linux" && file.exists("/etc/os-release")) {
    lines <- readLines("/etc/os-release")
    pretty <- grep("^PRETTY_NAME=", lines, value = TRUE)
    if (length(pretty)) {
        expected <- sub("\"(.*)\"", "\\1", sub("^PRETTY_NAME=", "", pretty[[1L]]))
        stopifnot(identical(utils::osVersion, expected))
    }
}
cat(is.character(utils::osVersion), length(utils::osVersion), "\n")
