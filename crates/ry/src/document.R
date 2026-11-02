# `ry document`: runs roxygen2 on a package the way `devtools::document()` does, with the package
# `load_all()` loaded lazily, so roxygen2 forces only the definitions it documents.

local({
  function(root, roclets) {
    package <- read.dcf(file.path(root, "DESCRIPTION"), fields = "Package")[[1L]]
    message("ℹ Updating ", package, " documentation")
    # roxygen2 rewrites NAMESPACE's imports before it loads the code, so the code is loaded
    # (reloaded, when only that changed) when it asks.
    load_code <- function(path) {
      load_all(path, quiet = TRUE)
      asNamespace(package)
    }
    roxygen2::roxygenise(root, roclets, load_code = load_code)
    invisible(TRUE)
  }
})
