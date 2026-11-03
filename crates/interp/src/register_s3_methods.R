# SPDX-License-Identifier: GPL-2.0-or-later
#
# A modified copy of `registerS3methods()` from GNU R 4.3.3, which keeps its copyright notice:
#
# GNU R: src/library/base/R/namespace.R (registerS3methods)
#     Copyright (C) 1995-2023 The R Core Team
#
# Modifications Copyright (C) 2026 Felix Andreas. This file is free software: you can
# redistribute it and/or modify it under the terms of the GNU General Public License as
# published by the Free Software Foundation, either version 2 of the License, or (at your
# option) any later version.
#
# The runtime installs this in place of base R's own `registerS3methods()` only when that one is
# the GNU R 4.3.3 function this copies (`session::install_register_s3_methods`). The two loops
# that look at every method run natively (`ryLocalS4Generics`, `ryRegisterS3Methods` in
# `envir.rs`); every other step is GNU R's.
function(info, package, env)
{
    n <- NROW(info)
    if (n == 0L)
        return()
    methname <- paste(info[, 1], info[, 2], sep = ".")
    z <- is.na(info[, 3])
    info[z, 3] <- methname[z]
    if (ncol(info) == 3L)
        info <- cbind(info, NA_character_)
    Info <- cbind(info[, 1L:3L, drop = FALSE], methname, info[, 4L])
    loc <- names(env)
    if (any(notex <- match(info[, 3], loc, nomatch = 0L) == 0L)) {
        warning(sprintf(ngettext(sum(notex), "S3 method %s was declared in NAMESPACE but not found",
            "S3 methods %s were declared in NAMESPACE but not found"),
            paste(sQuote(info[notex, 3]), collapse = ", ")),
            call. = FALSE, domain = NA)
        Info <- Info[!notex, , drop = FALSE]
    }
    eager <- is.na(Info[, 5L])
    delayed <- Info[!eager, , drop = FALSE]
    Info <- Info[eager, , drop = FALSE]
    l2 <- localGeneric <- Info[, 1] %in% loc
    if (.isMethodsDispatchOn()) {
        local <- which(localGeneric)
        generic <- .Internal(ryLocalS4Generics(Info[local, 1], env,
            function(genfun) methods::is(genfun, "genericFunction")))
        for (i in local[generic]) {
            localGeneric[i] <- FALSE
            registerS3method(Info[i, 1], Info[i, 2], Info[i, 3], env)
        }
    }
    if (any(localGeneric)) {
        lin <- Info[localGeneric, , drop = FALSE]
        S3MethodsTable <- env[[".__S3MethodsTable__."]]
        .Internal(importIntoEnv(S3MethodsTable, lin[, 4], env, lin[, 3]))
    }
    fin <- Info[!l2, , drop = FALSE]
    overwrite <- matrix(.Internal(ryRegisterS3Methods(fin, env, environment(),
        quote(get(method, envir = home)), list(
            namespace = function(w) asNamespace(w),
            missing = function(genname)
                stop(gettextf("object '%s' not found whilst loading namespace '%s'",
                    genname, package), call. = FALSE, domain = NA),
            generic = function(genfun) methods::is(genfun, "genericFunction"),
            default = function(genfun) genfun@default,
            table = function(defenv) {
                table <- new.env(hash = TRUE, parent = baseenv())
                defenv[[".__S3MethodsTable__."]] <- table
                table
            },
            existing = function(table, nm) tryCatch(table[[nm]], error = function(e) NULL),
            home = function(e) environmentName(environment(e))
        ))), ncol = 2L, byrow = TRUE)
    if (package != "MASS" && nrow(overwrite)) {
        .fmt <- function(o) {
            sprintf("  %s %s", format(c("method", o[, 1L])),
                format(c("from", o[, 2L])))
        }
        overwrite <- overwrite[overwrite[, 2L] != package, , drop = FALSE]
        if (Sys.getenv("_R_LOAD_CHECK_OVERWRITE_S3_METHODS_") %in% c(package, "all")) {
            ind <- overwrite[, 2L] %in% unlist(tools:::.get_standard_package_names(),
                use.names = FALSE)
            bad <- overwrite[ind, , drop = FALSE]
            if (nr <- nrow(bad)) {
                msg <- ngettext(nr, "Registered S3 method from a standard package overwritten by '%s':",
                    "Registered S3 methods from standard package(s) overwritten by '%s':",
                    domain = NA)
                msg <- paste(c(sprintf(msg, package), .fmt(bad)), collapse = "\n")
                message(msg, domain = NA)
                overwrite <- overwrite[!ind, , drop = FALSE]
            }
        }
        if ((nr <- nrow(overwrite)) &&
            is.na(match(tolower(Sys.getenv("_R_S3_METHOD_REGISTRATION_NOTE_OVERWRITES_")),
                c("0", "no", "false"))) &&
            (!is.na(match(Sys.getenv("_R_CHECK_PACKAGE_NAME_"), c("", package))))) {
            msg <- ngettext(nr, "Registered S3 method overwritten by '%s':",
                "Registered S3 methods overwritten by '%s':", domain = NA)
            msg <- paste(c(sprintf(msg, package), .fmt(overwrite)), collapse = "\n")
            packageStartupMessage(msg, domain = NA)
        }
    }
    register_S3_method_delayed <- function(pkg, gen, cls, fun) {
        pkg <- pkg
        gen <- gen
        cls <- cls
        fun <- fun
        if (isNamespaceLoaded(pkg)) {
            registerS3method(gen, cls, fun, envir = asNamespace(pkg))
        }
        setHook(packageEvent(pkg, "onLoad"), function(...) {
            registerS3method(gen, cls, fun, envir = asNamespace(pkg))
        })
    }
    if (nrow(delayed)) {
        for (i in seq_len(nrow(delayed))) {
            gen <- delayed[i, 1L]
            cls <- delayed[i, 2L]
            fun <- get(delayed[i, 3L], envir = env)
            pkg <- delayed[i, 5L]
            register_S3_method_delayed(pkg, gen, cls, fun)
        }
    }
    nsI <- getNamespaceInfo(env, "S3methods")
    if (!is.null(p1 <- ncol(nsI)) && !is.null(p2 <- ncol(info)) && p1 != p2)
        stop(gettextf(paste("While loading namespace \"%s\": \"%s\" differ in ncol(.), env=%d, newNS=%d.",
            "Maybe package installed with version of R newer than %s ?", sep = "\n"),
            package, "S3methods", p1, p2, getRversion()), domain = NA)
    setNamespaceInfo(env, "S3methods", rbind(info, nsI))
}
