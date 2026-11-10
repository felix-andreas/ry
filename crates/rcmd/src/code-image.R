# Writes the compiled code image of an installed package: loads it, fetches every lazy-loaded
# binding of its namespace so that each closure's record is recorded, optionally runs a training
# workload, and has the runtime compile and write the code of every recorded closure.
#
# Arguments: the package, the library it is installed in, its source directory, the training
# workload ("none", "tests", "examples", or the path of a script), and whether to print what kept
# each function from machine code ("true" or "false").
arguments <- commandArgs(TRUE)
package <- arguments[[1L]]
library <- arguments[[2L]]
source_directory <- arguments[[3L]]
training <- arguments[[4L]]
explain <- arguments[[5L]] == "true"

.Internal(ryCodeImageRecord(file.path(library, package)))
namespace <- loadNamespace(package, lib.loc = library)
for (name in ls(namespace, all.names = TRUE)) {
    if (!bindingIsActive(name, namespace)) {
        tryCatch(get(name, envir = namespace, inherits = FALSE), error = function(condition) NULL)
    }
}

# Runs each file in a fresh environment with its output discarded. A file that fails stops only
# itself: training gathers feedback, it does not check the package.
run_quietly <- function(files, chdir) {
    discard <- file(nullfile(), open = "w")
    sink(discard)
    sink(discard, type = "message")
    on.exit({
        sink(type = "message")
        sink()
        close(discard)
    })
    for (file in files) {
        environment <- new.env(parent = globalenv())
        tryCatch(
            suppressWarnings(source(file, local = environment, chdir = chdir)),
            error = function(condition) NULL
        )
    }
}

trained <- training != "none"
if (training == "tests") {
    files <- list.files(file.path(source_directory, "tests"), pattern = "\\.[Rr]$", full.names = TRUE)
    suppressPackageStartupMessages(base::library(package, lib.loc = library, character.only = TRUE))
    run_quietly(files, chdir = TRUE)
} else if (training == "examples") {
    files <- tryCatch({
        database <- tools::Rd_db(dir = source_directory)
        vapply(database, function(rd) {
            file <- tempfile(fileext = ".R")
            tools::Rd2ex(rd, file)
            file
        }, "")
    }, error = function(condition) character())
    suppressPackageStartupMessages(base::library(package, lib.loc = library, character.only = TRUE))
    run_quietly(files[file.exists(files)], chdir = FALSE)
} else if (trained) {
    suppressPackageStartupMessages(base::library(package, lib.loc = library, character.only = TRUE))
    run_quietly(training, chdir = TRUE)
}

counts <- .Internal(ryCodeImageWrite(trained, explain))
summary <- sprintf(
    "closures=%d written=%d native=%d",
    counts[["closures"]], counts[["written"]], counts[["native"]]
)
left_out <- counts[-(1:3)]
if (length(left_out)) {
    summary <- sprintf("%s (left out: %s)", summary, paste0(names(left_out), " (", left_out, ")", collapse = ", "))
}
cat(summary, "\n", sep = "")
