# The R-level half of `R CMD INSTALL`, run on ry's interpreter by `rcmd::install` after it has
# configured and compiled the package. It follows the order of GNU R's installer
# (`tools:::.install_packages`) and calls the same `tools` functions, so the installed layout
# is GNU R's: `DESCRIPTION` and `Meta/package.rds`, the collated `R/<pkg>` code turned into a
# lazy-load database, `sysdata`, lazy data, demos, `inst/`, the Rd database and help indices,
# and `Meta/nsInfo.rds`. Nothing is byte-compiled: ry compiles closures itself when they run.
#
# Arguments: the source directory (already configured and compiled), the staging library the
# package installs into, the final installation directory, and flags (`--install-tests`,
# `--no-docs`, `--no-data`, `--no-inst`, `--no-demo`, `--no-exec`, `--no-R`, and
# `--install-libs`, which runs the package's `src/install.libs.R`).

local({
    arguments <- commandArgs(TRUE)
    source_dir <- normalizePath(arguments[[1L]])
    lib <- normalizePath(arguments[[2L]])
    final_instdir <- arguments[[3L]]
    flags <- arguments[-(1:3)]
    has_flag <- function(flag) flag %in% flags

    options(warn = 1L, keep.source = FALSE)
    Sys.setenv(R_ENABLE_JIT = "0")
    `%notin%` <- function(x, table) !(x %in% table)
    stars <- "**"
    step <- function(...) cat(stars, " ", ..., "\n", sep = "")
    fail <- function(...) {
        message("ERROR: ", ...)
        q("no", status = 1L, runLast = FALSE)
    }
    attempt <- function(expression, what) {
        result <- tryCatch(expression, error = function(e) {
            call <- conditionCall(e)
            where <- if (is.null(call)) "" else paste0(" in ", paste(deparse(call, nlines = 1L), collapse = ""))
            fail(what, where, ": ", conditionMessage(e))
        })
        invisible(result)
    }
    parse_description_field <- function(desc, field, default) {
        tools:::str_parse_logic(desc[field], default = default,
                                otherwise = quote(fail("invalid value of ", field, " field in DESCRIPTION")))
    }
    vc_dirs <- "(^|/)(\\.svn|\\.arch-ids|CVS|\\.git|\\.hg|_darcs|\\.bzr)(/|$)"

    setwd(source_dir)
    desc <- tools:::.read_description("DESCRIPTION")
    pkg_name <- desc[["Package"]]
    instdir <- file.path(lib, pkg_name)
    dir.create(instdir, recursive = TRUE, showWarnings = FALSE)

    for (f in c("NAMESPACE", "LICENSE", "LICENCE", "NEWS", "NEWS.md")) {
        if (file.exists(f)) file.copy(f, instdir, overwrite = TRUE)
    }
    attempt(tools:::.install_package_description(".", instdir), "installing package DESCRIPTION failed")
    if (!file.exists(file.path(instdir, "NAMESPACE"))) {
        if (dir.exists("R")) fail("a 'NAMESPACE' file is required")
        writeLines("## package without R code", file.path(instdir, "NAMESPACE"))
    }

    if (has_flag("--install-libs")) {
        step("libs")
        message("installing via 'install.libs.R' to ", instdir)
        scope <- new.env(parent = .GlobalEnv)
        assign("SHLIB_EXT", ".so", envir = scope)
        assign("R_PACKAGE_DIR", instdir, envir = scope)
        assign("R_PACKAGE_NAME", pkg_name, envir = scope)
        assign("R_PACKAGE_SOURCE", source_dir, envir = scope)
        assign("R_ARCH", "", envir = scope)
        assign("WINDOWS", FALSE, envir = scope)
        setwd("src")
        attempt(source("install.libs.R", local = scope), "running 'src/install.libs.R' failed")
        setwd(source_dir)
    }

    install_r <- !has_flag("--no-R") && dir.exists("R") && length(dir("R"))
    if (install_r) {
        step("R")
        dir.create(file.path(instdir, "R"), recursive = TRUE, showWarnings = FALSE)
        attempt(tools:::.install_package_code_files(".", instdir), "unable to collate and parse R files")
        if (file.exists(f <- file.path("R", "sysdata.rda"))) {
            compress <- TRUE
            if (!is.na(method <- desc["SysDataCompression"])) {
                compress <- switch(method, none = FALSE, gzip = TRUE, bzip2 = 2L, xz = 3L, TRUE)
            } else if (file.size(f) > 1e6) {
                compress <- 3L
            }
            attempt(tools:::sysdata2LazyLoadDB(f, file.path(instdir, "R"), compress = compress),
                    "unable to build sysdata DB")
        }
    }

    if (!has_flag("--no-data") && dir.exists("data") && length(dir("data"))) {
        step("data")
        files <- Sys.glob(file.path("data", "*"))
        target <- file.path(instdir, "data")
        dir.create(target, recursive = TRUE, showWarnings = FALSE)
        file.copy(files, target, overwrite = TRUE, recursive = TRUE)
        lazy <- parse_description_field(desc, "LazyData", default = FALSE)
        if (lazy) {
            step("*  moving datasets to lazyload DB")
            compress <- TRUE
            if (!is.na(method <- desc["LazyDataCompression"])) {
                compress <- switch(method, none = FALSE, gzip = TRUE, bzip2 = 2L, xz = 3L, TRUE)
            }
            attempt(tools:::data2LazyLoadDB(pkg_name, lib, compress = compress), "lazydata failed")
        }
    }

    if (!has_flag("--no-demo") && dir.exists("demo") && length(dir("demo"))) {
        step("demo")
        dir.create(file.path(instdir, "demo"), recursive = TRUE, showWarnings = FALSE)
        attempt(tools:::.install_package_demos(".", instdir), "installing demos failed")
    }

    if (!has_flag("--no-exec") && dir.exists("exec") && length(dir("exec"))) {
        step("exec")
        dir.create(file.path(instdir, "exec"), recursive = TRUE, showWarnings = FALSE)
        files <- Sys.glob(file.path("exec", "*"))
        file.copy(files, file.path(instdir, "exec"), overwrite = TRUE)
        Sys.chmod(Sys.glob(file.path(instdir, "exec", "*")), "755")
    }

    if (!has_flag("--no-inst") && dir.exists("inst") && length(dir("inst", all.files = TRUE)) > 2L) {
        step("inst")
        ignore <- if (file.exists(".Rinstignore")) {
            patterns <- readLines(".Rinstignore", warn = FALSE)
            patterns[nzchar(patterns)]
        } else character()
        keep <- function(paths) {
            paths <- paths[!grepl(vc_dirs, paths)]
            for (pattern in ignore) paths <- paths[!grepl(pattern, paths, perl = TRUE, ignore.case = TRUE)]
            paths
        }
        directories <- keep(list.dirs("inst")[-1L])
        for (directory in sub("^inst", instdir, directories)) dir.create(directory, showWarnings = FALSE, recursive = TRUE)
        files <- keep(list.files("inst", all.files = TRUE, full.names = TRUE, recursive = TRUE))
        files <- files[files %notin% c("inst/doc/Rplots.pdf", "inst/doc/Rplots.ps", "inst/Makefile")]
        files <- files[!grepl("inst/doc/.*[.](log|aux|bbl|blg|dvi)$", files, perl = TRUE, ignore.case = TRUE)]
        if (!dir.exists("vignettes")) {
            files <- files[!grepl("inst/doc/.*[.](png|jpg|jpeg|gif|ps|eps)$", files, perl = TRUE, ignore.case = TRUE)]
        }
        targets <- sub("^inst", instdir, files)
        file.copy(files, targets, overwrite = TRUE)
        executable <- as.logical(file.mode(files) & as.octmode("100"))
        if (any(executable, na.rm = TRUE)) Sys.chmod(targets[which(executable)], "755")
    }

    if (has_flag("--install-tests") && dir.exists("tests") && length(dir("tests", all.files = TRUE)) > 2L) {
        step("tests")
        file.copy("tests", instdir, recursive = TRUE)
    }

    if (install_r) {
        step("preparing package for lazy loading")
        keep_source <- parse_description_field(desc, "KeepSource", default = FALSE)
        if (isNamespaceLoaded(pkg_name)) unloadNamespace(pkg_name)
        .libPaths(c(lib, .libPaths()))
        attempt({
            suppressPackageStartupMessages(.getRequiredPackages(file.path(instdir, "DESCRIPTION"), quietly = TRUE))
            tools:::makeLazyLoading(pkg_name, lib, keep.source = keep_source,
                                    keep.parse.data = FALSE, set.install.dir = final_instdir)
        }, "lazy loading failed")
    }

    if (!has_flag("--no-docs")) {
        step("help")
        if (!dir.exists("man") || !length(tools:::list_files_with_type("man", "docs"))) {
            cat("No man pages found in package ", sQuote(pkg_name), "\n")
        }
        encoding <- desc["Encoding"]
        if (is.na(encoding)) encoding <- "unknown"
        attempt(tools:::.install_package_Rd_objects(".", instdir, encoding), "installing Rd objects failed")
        step("*  installing help indices")
        attempt(tools:::.writePkgIndices(".", instdir), "installing help indices failed")
        if (dir.exists(figures <- file.path("man", "figures"))) {
            dir.create(target <- file.path(instdir, "help", "figures"), showWarnings = FALSE)
            file.copy(Sys.glob(file.path(figures, c("*.png", "*.jpg", "*.jpeg", "*.svg", "*.pdf"))), target)
        }
    }

    step("building package indices")
    attempt(tools:::.install_package_indices(".", instdir), "installing package indices failed")
    if (dir.exists("vignettes")) {
        step("installing vignettes")
        encoding <- desc["Encoding"]
        if (is.na(encoding)) encoding <- ""
        installer <- if (file.exists(file.path("build", "vignette.rds"))) {
            tools:::.install_package_vignettes3
        } else tools:::.install_package_vignettes2
        attempt(installer(".", instdir, encoding), "installing vignettes failed")
    }

    if (file.exists("NAMESPACE")) {
        attempt(tools:::.install_package_namespace_info(".", instdir), "installing namespace metadata failed")
    }
})
