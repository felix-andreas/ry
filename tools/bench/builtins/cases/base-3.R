bench("isS4", scalar = isS4(dbl_1), df_10 = isS4(df_10))
bench("isSeekable", setup = { con <- stdin() }, small = isSeekable(con))
bench("isSymmetric", mat_100 = isSymmetric(spd_100), mat_100_asym = isSymmetric(mat_100), mat_10 = isSymmetric(mat_10))
bench("isSymmetric.matrix", mat_100 = isSymmetric.matrix(spd_100))
bench("isTRUE", scalar = isTRUE(lgl_1), dbl_1 = isTRUE(dbl_1), lgl_1e3 = isTRUE(lgl_1e3))
bench("jitter", dbl_1e3 = jitter(dbl_1e3), dbl_1e6 = jitter(dbl_1e6))
bench("julian", date_1e3 = julian(date_1e3), time_1e3 = julian(time_1e3), date_1e6 = julian(date_1e6))
bench("julian.Date", date_1e3 = julian.Date(date_1e3), date_1e6 = julian.Date(date_1e6))
bench("julian.POSIXt", time_1e3 = julian.POSIXt(time_1e3), time_1e6 = julian.POSIXt(time_1e6))
bench("kappa", mat_10 = kappa(mat_10), mat_100 = kappa(mat_100), exact_100 = kappa(mat_100, exact = TRUE))
bench("kappa.default", mat_100 = kappa.default(mat_100))
bench("kappa.lm", setup = { fit <- lm(y ~ x + g, model_1e3) }, model_1e3 = kappa.lm(fit))
bench("kappa.qr", setup = { q <- qr(mat_100) }, mat_100 = kappa.qr(q))
bench(".kappa_tri", setup = { r <- qr.R(qr(mat_100)) }, mat_100 = .kappa_tri(r), exact_100 = .kappa_tri(r, exact = TRUE))
bench(".kronecker", mat_10 = .kronecker(mat_10, mat_10))
bench("kronecker", mat_10 = kronecker(mat_10, mat_10), mat_100 = kronecker(mat_100, mat_10),
      fun_10 = kronecker(mat_10, mat_10, "+"))
bench("l10n_info", small = l10n_info())
bench("La.svd", mat_10 = La.svd(mat_10), mat_100 = La.svd(mat_100))
bench("La_library", small = La_library())
bench("La_version", small = La_version())
bench("labels", setup = { fit <- lm(y ~ x + g, model_1e3) }, named_1e3 = labels(named_1e3), dbl_1e3 = labels(dbl_1e3),
      df_10 = labels(df_10), lm = labels(fit))
bench("labels.default", named_1e3 = labels.default(named_1e3))
bench("lapply", setup = { lst_1e5 <- lst_1e6[seq_len(1e5)] },
      lst_10 = lapply(lst_10, length), lst_1e3 = lapply(lst_1e3, fun), dbl_1e3 = lapply(dbl_1e3, fun),
      builtin_1e3 = lapply(lst_1e3, is.numeric), df_1e3 = lapply(df_1e3, class), lst_1e5 = lapply(lst_1e5, fun))
skip("lazyLoad", "reads files")
skip("lazyLoadDBexec", "reads files")
skip("lazyLoadDBfetch", "reads files")
bench("lbeta", scalar = lbeta(dbl_1, dbl_1), pos_1e3 = lbeta(pos_1e3, pos_1e3), pos_1e6 = lbeta(pos_1e6, pos_1e6))
bench("lchoose", scalar = lchoose(int_1, 3L), int_1e3 = lchoose(int_1e3, 5L), pos_1e3 = lchoose(pos_1e3, 3),
      int_1e6 = lchoose(int_1e6, 5L))
bench("...length", setup = { f <- function(...) ...length() }, small = f(dbl_1, chr_1, lgl_1))
bench("length", scalar = length(dbl_1), dbl_1e3 = length(dbl_1e3), lst_1e3 = length(lst_1e3),
      df_1e3 = length(df_1e3), env_1e3 = length(env_1e3), dbl_1e6 = length(dbl_1e6))
bench("length.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = length.POSIXlt(lt))
bench("length<-", setup = { y <- dbl_1e3; l <- lst_1e3 },
      shrink_1e3 = { z <- y; length(z) <- 500L; z }, grow_1e3 = { z <- y; length(z) <- 2000L; z },
      lst_1e3 = { z <- l; length(z) <- 500L; z })
bench("length<-.Date", setup = { y <- date_1e3 }, date_1e3 = { z <- y; length(z) <- 500L; z })
bench("length<-.difftime", setup = { d <- as.difftime(dbl_1e3, units = "secs") }, difftime_1e3 = { z <- d; length(z) <- 500L; z })
bench("length<-.factor", setup = { y <- fct_1e3 }, fct_1e3 = { z <- y; length(z) <- 500L; z })
bench("length<-.POSIXct", setup = { y <- time_1e3 }, time_1e3 = { z <- y; length(z) <- 500L; z })
bench("length<-.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = `length<-.POSIXlt`(lt, 500L))
bench("lengths", lst_10 = lengths(lst_10), lst_1e3 = lengths(lst_1e3), df_1e3 = lengths(df_1e3),
      chr_1e3 = lengths(chr_1e3), lst_1e6 = lengths(lst_1e6))
bench("levels", fct_1e3 = levels(fct_1e3), dbl_1 = levels(dbl_1), fct_1e6 = levels(fct_1e6))
bench("levels.default", fct_1e3 = levels.default(fct_1e3))
bench("levels<-", setup = { y <- fct_1e3; big <- fct_1e6; new <- toupper(levels(y)) },
      fct_1e3 = levels(y) <- new, fct_1e6 = levels(big) <- new)
bench("levels<-.factor", setup = { new <- toupper(levels(fct_1e3)) }, fct_1e3 = `levels<-.factor`(fct_1e3, new))
bench("lfactorial", scalar = lfactorial(int_1), pos_1e3 = lfactorial(pos_1e3), pos_1e6 = lfactorial(pos_1e6))
bench("lgamma", scalar = lgamma(dbl_1), pos_1e3 = lgamma(pos_1e3), pos_1e6 = lgamma(pos_1e6))
bench("libcurlVersion", small = libcurlVersion())
bench(".libPaths", small = .libPaths())
skip("library", "changes the session")
skip("library.dynam", "changes the session")
skip("library.dynam.unload", "changes the session")
bench("licence", small = licence())
bench("license", small = license())
bench("list", small = list(dbl_1, chr_1, lgl_1), named = list(a = dbl_1, b = chr_1, c = lgl_1),
      vectors_1e3 = list(dbl_1e3, chr_1e3), empty = list(), ten = list(dbl_1, dbl_1, dbl_1, dbl_1, dbl_1,
                                                                      dbl_1, dbl_1, dbl_1, dbl_1, dbl_1))
skip("list.dirs", "reads files")
skip("list.files", "reads files")
bench("list2DF", setup = { l <- list(x = dbl_1e3, y = chr_1e3, z = int_1e3); big <- list(x = dbl_1e6, y = chr_1e6) },
      lst_1e3 = list2DF(l), lst_1e6 = list2DF(big))
bench("list2env", setup = { l <- as.list(env_1e3) }, small = list2env(lst_10), lst_1e3 = list2env(l))
skip("load", "reads files")
bench("loadedNamespaces", small = loadedNamespaces())
skip("loadingNamespaceInfo", "internal")
bench("loadNamespace", setup = { p <- "stats" }, loaded = loadNamespace(p))
bench("local", small = local(dbl_1 + 1), block = local({ a <- dbl_1; a * 2 }))
bench("lockBinding", setup = { e <- new.env(); assign("a", 1, envir = e) }, small = lockBinding("a", e))
bench("lockEnvironment", setup = { e <- new.env(); e2 <- list2env(as.list(env_1e3)) },
      small = lockEnvironment(e), bindings_1e3 = lockEnvironment(e2, bindings = TRUE))
bench("log", scalar = log(dbl_1), pos_1e3 = log(pos_1e3), base_1e3 = log(pos_1e3, 10), pos_1e6 = log(pos_1e6),
      cpl_1e3 = log(cpl_1e3))
bench("log10", scalar = log10(dbl_1), pos_1e3 = log10(pos_1e3), pos_1e6 = log10(pos_1e6))
bench("log1p", scalar = log1p(dbl_1), pos_1e3 = log1p(pos_1e3), pos_1e6 = log1p(pos_1e6))
bench("log2", scalar = log2(dbl_1), pos_1e3 = log2(pos_1e3), pos_1e6 = log2(pos_1e6))
bench("logb", scalar = logb(dbl_1), pos_1e3 = logb(pos_1e3))
bench("logical", setup = { n <- 1000L; m <- 1000000L }, scalar = logical(int_1), n_1e3 = logical(n), n_1e6 = logical(m))
bench("lower.tri", mat_100 = lower.tri(mat_100), mat_1000 = lower.tri(mat_1000), diag_100 = lower.tri(mat_100, diag = TRUE))
bench("ls", setup = { e <- list2env(lst_10) }, small = ls(e), env_1e3 = ls(env_1e3), unsorted_1e3 = ls(env_1e3, sorted = FALSE),
      all_names = ls(e, all.names = TRUE))
bench("make.names", scalar = make.names(txt_1), chr_1e3 = make.names(chr_1e3), txt_1e3 = make.names(txt_1e3),
      unique_1e3 = make.names(chr_1e3, unique = TRUE))
bench("make.unique", setup = { s <- as.character(fct_1e3); big <- as.character(fct_1e6[seq_len(1e5)]) },
      chr_1e3 = make.unique(chr_1e3), dup_1e3 = make.unique(s), dup_1e5 = make.unique(big))
bench(".make_numeric_version", setup = { v <- sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3) },
      chr_1e3 = .make_numeric_version(v, TRUE, .standard_regexps()$valid_numeric_version))
bench("makeActiveBinding", setup = { e <- new.env(); f <- function() 1 }, small = makeActiveBinding("a", f, e))
bench(".makeMessage", small = .makeMessage(chr_1, txt_1, dbl_1), domain = .makeMessage(txt_1, domain = NA))
bench("Map", setup = { f2 <- function(x, y) x + y }, dbl_1e3 = Map(f2, dbl_1e3, dbl_1e3), lst_1e3 = Map(f2, lst_1e3, 1))
bench(".mapply", setup = { f2 <- function(x, y) x + y }, dbl_1e3 = .mapply(f2, list(dbl_1e3, dbl_1e3), NULL))
bench("mapply", setup = { f2 <- function(x, y) x + y },
      dbl_1e3 = mapply(f2, dbl_1e3, dbl_1e3), lst_1e3 = mapply(f2, lst_1e3, lst_1e3, SIMPLIFY = FALSE),
      chr_1e3 = mapply(paste, chr_1e3, chr_1e3, USE.NAMES = FALSE), small = mapply(rep, 1:3, int_1))
bench("margin.table", setup = { t <- table(df_1e3$g, df_1e3$b) }, table_1 = margin.table(t, 1), total = margin.table(t))
bench("marginSums", setup = { t <- table(df_1e3$g, df_1e3$b) }, table_2 = marginSums(t, 2), mat_100 = marginSums(mat_100, 1))
bench(".maskedMsg", setup = { same <- c("filter", "lag") }, small = .maskedMsg(same, "stats", TRUE))
bench("mat.or.vec", setup = { n <- 100L }, mat_100 = mat.or.vec(n, n), vec_1e3 = mat.or.vec(1000L, int_1 - 6L))
bench("match", setup = { ti <- int_1e3[1:100]; tc <- chr_1e3[1:100]; tb <- chr_1e6[1:1000] },
      scalar = match(int_1, int_1e3), int_1e3 = match(int_1e3, ti), chr_1e3 = match(chr_1e3, tc),
      dbl_1e3 = match(dbl_1e3, dbl_1e3), fct_1e3 = match(fct_1e3, letters), int_1e6 = match(int_1e6, ti),
      chr_1e6 = match(chr_1e6, tb))
bench("match.arg", setup = {
  f <- function(type = c("linear", "quadratic", "cubic")) match.arg(type)
  g <- function(type = c("linear", "quadratic", "cubic")) match.arg(type, several.ok = TRUE)
  t <- "quad"
}, default = f(), given = f(t), several = g(c(t, "cub")), explicit = match.arg(t, c("linear", "quadratic")))
bench("match.call", setup = { f <- function(x, ...) match.call(); g <- function(x, y, ...) NULL; cl <- quote(g(1, y = 2, 3)) },
      small = f(dbl_1, y = chr_1), explicit = match.call(g, cl), expand = f(dbl_1, chr_1, lgl_1, z = 2))
bench("match.fun", setup = { s <- "sum" }, fun = match.fun(fun), string = match.fun(s))
bench("Math.data.frame", setup = { d <- df_1e3[c("x", "y")] }, abs_1e3 = abs(d), round_1e3 = round(d, 1))
skip("Math.Date", "only stops with an error")
bench("Math.difftime", setup = { d <- as.difftime(dbl_1e3, units = "secs") }, abs_1e3 = abs(d), round_1e3 = round(d))
skip("Math.factor", "only stops with an error")
skip("Math.POSIXt", "only stops with an error")
bench("matrix", setup = { n <- 100L },
      scalar = matrix(dbl_1, n, n), dbl_1e3 = matrix(dbl_1e3, 100L), byrow_1e3 = matrix(dbl_1e3, 100L, byrow = TRUE),
      chr_1e3 = matrix(chr_1e3, ncol = 10L), dimnames = matrix(dbl_1e3[1:4], 2L, dimnames = list(c("a", "b"), c("c", "d"))),
      dbl_1e6 = matrix(dbl_1e6, 1000L))
bench("max", scalar = max(dbl_1, int_1), dbl_1e3 = max(dbl_1e3), int_1e3 = max(int_1e3), na_1e3 = max(na_1e3, na.rm = TRUE),
      chr_1e3 = max(chr_1e3), two_1e3 = max(dbl_1e3, pos_1e3), dbl_1e6 = max(dbl_1e6), int_1e6 = max(int_1e6))
bench("max.col", mat_100 = max.col(mat_100), first_100 = max.col(mat_100, "first"), mat_1000 = max.col(mat_1000))
bench("mean", scalar = mean(dbl_1), dbl_1e3 = mean(dbl_1e3), int_1e3 = mean(int_1e3), na_1e3 = mean(na_1e3, na.rm = TRUE),
      trim_1e3 = mean(dbl_1e3, trim = 0.1), lgl_1e3 = mean(lgl_1e3), dbl_1e6 = mean(dbl_1e6), int_1e6 = mean(int_1e6))
bench("mean.Date", date_1e3 = mean.Date(date_1e3), date_1e6 = mean.Date(date_1e6))
bench("mean.default", dbl_1e3 = mean.default(dbl_1e3), dbl_1e6 = mean.default(dbl_1e6))
bench("mean.difftime", setup = { d <- as.difftime(dbl_1e3, units = "secs") }, difftime_1e3 = mean.difftime(d))
bench("mean.POSIXct", time_1e3 = mean.POSIXct(time_1e3), time_1e6 = mean.POSIXct(time_1e6))
bench("mean.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = mean.POSIXlt(lt))
skip("mem.maxNSize", "nondeterministic")
skip("mem.maxVSize", "nondeterministic")
bench("memCompress", setup = { bytes <- charToRaw(paste(txt_1e3, collapse = " ")) },
      gzip_txt_1e3 = memCompress(bytes, "gzip"), bzip2_txt_1e3 = memCompress(bytes, "bzip2"),
      xz_txt_1e3 = memCompress(bytes, "xz"), raw_1e3 = memCompress(raw_1e3, "gzip"))
bench("memDecompress", setup = {
  bytes <- charToRaw(paste(txt_1e3, collapse = " "))
  gz <- memCompress(bytes, "gzip"); bz <- memCompress(bytes, "bzip2"); xz <- memCompress(bytes, "xz")
}, gzip_txt_1e3 = memDecompress(gz, "gzip"), bzip2_txt_1e3 = memDecompress(bz, "bzip2"),
   xz_txt_1e3 = memDecompress(xz, "xz"), unknown_txt_1e3 = memDecompress(gz, "unknown"))
skip("memory.profile", "nondeterministic")
bench("merge", setup = {
  a <- df_1e3[c("id", "x", "g")]
  b <- df_1e3[sample.int(1000L), c("id", "y")]
  k <- df_10[c("g", "s")]
}, df_1e3 = merge(a, b, by = "id"), factor_1e3 = merge(a, k, by = "g"), all_1e3 = merge(a, b[1:500, ], all = TRUE),
   nosort_1e3 = merge(a, b, by = "id", sort = FALSE))
bench("merge.data.frame", setup = { a <- df_1e3[c("id", "x")]; b <- df_1e3[sample.int(1000L), c("id", "y")] },
      df_1e3 = merge.data.frame(a, b, by = "id"))
bench("merge.default", setup = { a <- dbl_1e3[1:10]; b <- chr_1e3[1:10] }, small = merge.default(a, b))
skip(".mergeExportMethods", "internal")
skip(".mergeImportMethods", "internal")
bench("message", suppressed = suppressMessages(message(txt_1)), parts = suppressMessages(message(chr_1, ": ", dbl_1)))
bench("messageCondition", small = messageCondition(txt_1), classed = messageCondition(txt_1, class = "custom"))
bench("mget", setup = { some <- ls(env_1e3)[1:100]; all <- ls(env_1e3) },
      env_100 = mget(some, envir = env_1e3), env_1e3 = mget(all, envir = env_1e3))
bench("min", scalar = min(dbl_1, int_1), dbl_1e3 = min(dbl_1e3), int_1e3 = min(int_1e3), na_1e3 = min(na_1e3, na.rm = TRUE),
      chr_1e3 = min(chr_1e3), two_1e3 = min(dbl_1e3, pos_1e3), dbl_1e6 = min(dbl_1e6), int_1e6 = min(int_1e6))
bench("missing", setup = { f <- function(x) missing(x); g <- function(x, y) f(y) },
      absent = f(), present = f(dbl_1), nested = g(dbl_1))
bench("Mod", scalar = Mod(cpl_1), cpl_1e3 = Mod(cpl_1e3), cpl_1e6 = Mod(cpl_1e6), dbl_1e3 = Mod(dbl_1e3))
bench("mode", dbl_1 = mode(dbl_1), lst_10 = mode(lst_10), fun = mode(fun), formula = mode(formula_1))
bench("mode<-", setup = { y <- int_1e3 }, int_1e3 = { z <- y; mode(z) <- "double"; z },
      chr_1e3 = { z <- y; mode(z) <- "character"; z })
bench("months", date_1e3 = months(date_1e3), time_1e3 = months(time_1e3), abbr_1e3 = months(date_1e3, TRUE))
bench("months.Date", date_1e3 = months.Date(date_1e3))
bench("months.POSIXt", time_1e3 = months.POSIXt(time_1e3))
bench("mostattributes<-", setup = { y <- dbl_1e3; a <- list(names = chr_1e3, note = "x", class = "foo") },
      dbl_1e3 = { z <- y; mostattributes(z) <- a; z })
bench("mtfrm", dbl_1e3 = mtfrm(dbl_1e3), fct_1e3 = mtfrm(fct_1e3), date_1e3 = mtfrm(date_1e3))
bench("mtfrm.Date", date_1e3 = mtfrm.Date(date_1e3))
bench("mtfrm.default", dbl_1e3 = mtfrm.default(dbl_1e3), chr_1e3 = mtfrm.default(chr_1e3))
bench("mtfrm.POSIXct", time_1e3 = mtfrm.POSIXct(time_1e3))
bench("mtfrm.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = mtfrm.POSIXlt(lt))
bench("nameOfClass", df_10 = nameOfClass(df_10), fun = nameOfClass(fun))
bench("nameOfClass.default", df_10 = nameOfClass.default(df_10))
bench("...names", setup = { f <- function(...) ...names() }, small = f(a = dbl_1, chr_1, c = lgl_1))
bench("names", named_1e3 = names(named_1e3), dbl_1e3 = names(dbl_1e3), df_1e3 = names(df_1e3), lst_10 = names(lst_10),
      env_1e3 = names(env_1e3), named_1e6 = names(named_1e6))
bench("names.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = names.POSIXlt(lt))
bench("names<-", setup = { y <- dbl_1e3; l <- lst_10; big <- dbl_1e6; nm <- names(named_1e6) },
      dbl_1e3 = names(y) <- chr_1e3, lst_10 = names(l) <- letters[seq_along(l)], null_1e3 = { z <- y; names(z) <- NULL; z },
      dbl_1e6 = names(big) <- nm)
bench("names<-.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = `names<-.POSIXlt`(lt, chr_1e3))
skip("namespaceExport", "internal")
skip("namespaceImport", "internal")
skip("namespaceImportClasses", "internal")
skip("namespaceImportFrom", "internal")
skip("namespaceImportMethods", "internal")
bench("nargs", setup = { f <- function(a, b, c) nargs() }, small = f(dbl_1, dbl_1))
bench("nchar", scalar = nchar(chr_1), chr_1e3 = nchar(chr_1e3), txt_1e3 = nchar(txt_1e3), bytes_1e3 = nchar(txt_1e3, "bytes"),
      dbl_1e3 = nchar(dbl_1e3), txt_1e6 = nchar(txt_1e6))
bench("NCOL", dbl_1e3 = NCOL(dbl_1e3), mat_100 = NCOL(mat_100), df_1e3 = NCOL(df_1e3))
bench("ncol", mat_100 = ncol(mat_100), df_1e3 = ncol(df_1e3), dbl_1e3 = ncol(dbl_1e3))
bench("Negate", setup = { g <- is.na; nf <- Negate(is.na) }, small = Negate(g), na_1e3 = nf(na_1e3))
bench("new.env", setup = { p <- emptyenv() }, small = new.env(), parent = new.env(parent = p), sized = new.env(size = 1000L))
skip("next", "language construct")
bench("NextMethod", setup = {
  gen <- function(x, ...) UseMethod("gen")
  gen.child <- function(x, ...) NextMethod()
  gen.parent <- function(x, ...) NextMethod()
  gen.default <- function(x, ...) 1
  obj <- structure(dbl_1, class = c("child", "parent"))
}, small = gen(obj), args = gen(obj, dbl_1))
bench("ngettext", setup = { n <- 2L }, small = ngettext(n, "item", "items"))
bench("nlevels", fct_1e3 = nlevels(fct_1e3), dbl_1 = nlevels(dbl_1))
bench("noquote", chr_1e3 = noquote(chr_1e3), scalar = noquote(chr_1))
bench("norm", mat_100 = norm(mat_100), frobenius_100 = norm(mat_100, "F"), two_100 = norm(mat_100, "2"),
      inf_100 = norm(mat_100, "I"), mat_10 = norm(mat_10, "M"))
skip("normalizePath", "reads files")
skip(".NotYetImplemented", "only stops with an error")
skip(".NotYetUsed", "only stops with an error")
bench("NROW", dbl_1e3 = NROW(dbl_1e3), mat_100 = NROW(mat_100), df_1e3 = NROW(df_1e3), lst_10 = NROW(lst_10))
bench("nrow", mat_100 = nrow(mat_100), df_1e3 = nrow(df_1e3), dbl_1e3 = nrow(dbl_1e3), df_10 = nrow(df_10))
bench("nullfile", small = nullfile())
bench("numeric", setup = { n <- 1000L; m <- 1000000L }, scalar = numeric(int_1), n_1e3 = numeric(n), n_1e6 = numeric(m))
bench("numeric_version", setup = { v <- sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3); s <- "1.2.3" },
      scalar = numeric_version(s), chr_1e3 = numeric_version(v))
bench("numToBits", scalar = numToBits(dbl_1), dbl_1e3 = numToBits(dbl_1e3))
bench("numToInts", scalar = numToInts(dbl_1), dbl_1e3 = numToInts(dbl_1e3))
bench("nzchar", scalar = nzchar(chr_1), chr_1e3 = nzchar(chr_1e3), chr_1e6 = nzchar(chr_1e6))
bench("objects", env_1e3 = objects(env_1e3))
bench("oldClass", df_10 = oldClass(df_10), dbl_1 = oldClass(dbl_1), fct_1e3 = oldClass(fct_1e3))
bench("oldClass<-", setup = { y <- dbl_1e3 }, dbl_1e3 = oldClass(y) <- "foo", null_1e3 = { z <- df_10; oldClass(z) <- NULL; z })
skip("OlsonNames", "reads files")
bench("on.exit", setup = {
  f <- function(x) { on.exit(x + 1); x }
  g <- function(x) { on.exit(x + 1); on.exit(x + 2, add = TRUE); x }
}, small = f(dbl_1), add = g(dbl_1))
skip("open", "reads files")
skip("open.connection", "reads files")
skip("open.srcfile", "reads files")
bench("open.srcfilealias", setup = { sf <- srcfilecopy("x.R", txt_1e3); sa <- srcfilealias("y.R", sf) },
      txt_1e3 = { open.srcfilealias(sa, 10L); close(sa) })
bench("open.srcfilecopy", setup = { sf <- srcfilecopy("x.R", txt_1e3) }, txt_1e3 = { open.srcfilecopy(sf, 10L); close(sf) })
bench("Ops.data.frame", setup = { d <- df_1e3[c("x", "y")] }, plus_1e3 = d + d, scalar_1e3 = d * dbl_1, compare_1e3 = d > 0,
      df_10 = df_10[c("id", "x")] == 1)
bench("Ops.Date", date_1e3 = date_1e3 < date_1, equal_1e3 = date_1e3 == date_1e3, date_1e6 = date_1e6 >= date_1)
bench("Ops.difftime", setup = { d <- as.difftime(dbl_1e3, units = "secs"); e <- as.difftime(pos_1e3, units = "mins") },
      plus_1e3 = d + e, compare_1e3 = d < e, minus_1e3 = d - d)
bench("Ops.factor", fct_1e3 = fct_1e3 == "a", factors_1e3 = fct_1e3 != fct_1e3, fct_1e6 = fct_1e6 == "a")
bench("Ops.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3)); w <- numeric_version("3.1")
}, scalar = w >= "3.0.1", chr_1e3 = v < w)
bench("Ops.ordered", setup = { o <- factor(as.character(fct_1e3), levels = letters, ordered = TRUE) },
      fct_1e3 = o < "m", ordered_1e3 = o >= o)
bench("Ops.POSIXt", time_1e3 = time_1e3 > time_1, equal_1e3 = time_1e3 == time_1e3, time_1e6 = time_1e6 > time_1)
bench("options", setup = { k <- "digits"; n <- 7L }, get = options(k), set = options(digits = n), list = options("digits", "OutDec"))
skip(".OptRequireMethods", "internal")
bench("order", scalar = order(dbl_1), dbl_1e3 = order(dbl_1e3), int_1e3 = order(int_1e3), chr_1e3 = order(chr_1e3),
      sorted_1e3 = order(sorted_1e3), decreasing_1e3 = order(dbl_1e3, decreasing = TRUE),
      keys_1e3 = order(df_1e3$g, df_1e3$x), fct_1e3 = order(fct_1e3), radix_chr_1e3 = order(chr_1e3, method = "radix"),
      dbl_1e6 = order(dbl_1e6), int_1e6 = order(int_1e6), radix_chr_1e6 = order(chr_1e6, method = "radix"))
bench("ordered", setup = { s <- as.character(fct_1e3) }, chr_1e3 = ordered(s), levels_1e3 = ordered(s, levels = letters),
      fct_1e3 = ordered(fct_1e3))
bench("outer", setup = { x <- dbl_1e3[1:100]; f2 <- function(x, y) x * y },
      dbl_100 = outer(x, x), minus_100 = outer(x, x, "-"), fun_100 = outer(x, x, f2), dbl_1e3 = outer(dbl_1e3, dbl_1e3),
      chr_10 = outer(chr_1e3[1:10], letters, paste0))
bench("package_version", setup = { v <- sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3) },
      chr_1e3 = package_version(v), scalar = package_version(v[1L]))
bench("packageEvent", setup = { p <- "stats" }, small = packageEvent(p, "onLoad"))
skip("packageHasNamespace", "reads files")
bench("packageNotFoundError", setup = { p <- "nopkg" }, small = packageNotFoundError(p, NULL, NULL))
bench(".packages", setup = { all <- FALSE }, small = .packages(all))
bench("packageStartupMessage", suppressed = suppressPackageStartupMessages(packageStartupMessage(txt_1)))
bench("packBits", lgl_1e3 = packBits(lgl_1e3), int_1e3 = packBits(as.integer(lgl_1e3[1:960]), "integer"),
      lgl_1e6 = packBits(lgl_1e6))
bench("pairlist", small = pairlist(dbl_1, chr_1), named = pairlist(a = dbl_1, b = chr_1, c = lgl_1))
bench("parent.env", env_1e3 = parent.env(env_1e3), global = parent.env(globalenv()))
bench("parent.env<-", setup = { e <- new.env(); p <- emptyenv() }, small = parent.env(e) <- p)
bench("parent.frame", setup = { f <- function() parent.frame(); g <- function() f() },
      small = f(), nested = g(), up = (function() parent.frame(2L))())
bench("parse", setup = {
  s <- "x + 1"
  code <- deparse(stats::lm)
  many <- sprintf("f%d <- function(x) x * %d + sum(c(1, 2, %d))", seq_len(1000L), seq_len(1000L), seq_len(1000L))
}, small = parse(text = s), function_lm = parse(text = code), lines_1e3 = parse(text = many),
   keep_source = parse(text = code, keep.source = TRUE))
skip("parseNamespaceFile", "reads files")
bench("paste", setup = { chr_1e5 <- chr_1e6[seq_len(1e5)] }, scalar = paste(chr_1, dbl_1), chr_1e3 = paste(chr_1e3, chr_1e3), sep_1e3 = paste(chr_1e3, int_1e3, sep = "_"),
      collapse_1e3 = paste(chr_1e3, collapse = ","), dbl_1e3 = paste("x", dbl_1e3), recycle_1e3 = paste(chr_1, chr_1e3),
      chr_1e5 = paste(chr_1e5, "x"), collapse_1e5 = paste(chr_1e5, collapse = ""))
bench("paste0", setup = { chr_1e5 <- chr_1e6[seq_len(1e5)] }, scalar = paste0(chr_1, dbl_1), chr_1e3 = paste0(chr_1e3, chr_1e3), int_1e3 = paste0("v", int_1e3),
      collapse_1e3 = paste0(chr_1e3, collapse = ""), three_1e3 = paste0(chr_1e3, "_", fct_1e3),
      chr_1e5 = paste0(chr_1e5, "x"), int_1e6 = paste0("v", int_1e6))
bench("path.expand", setup = { p <- "~/x/y.R" }, scalar = path.expand(p), chr_1e3 = path.expand(chr_1e3))
bench("path.package", setup = { p <- "stats" }, small = path.package(p))
bench("pcre_config", small = pcre_config())
skip("pipe", "runs a process")
skip("plot", "graphics device")
bench("pmatch", setup = { s <- "me"; choices <- c("mean", "median", "mode"); abbr <- substr(chr_1e3[1:100], 1L, 6L) },
      scalar = pmatch(s, choices), exact = pmatch("median", choices), chr_100 = pmatch(abbr, chr_1e3),
      dup_100 = pmatch(abbr, chr_1e3, duplicates.ok = TRUE))
bench("pmax", scalar = pmax(dbl_1, 0), dbl_1e3 = pmax(dbl_1e3, pos_1e3), zero_1e3 = pmax(dbl_1e3, 0),
      int_1e3 = pmax(int_1e3, 500L), na_1e3 = pmax(na_1e3, 0, na.rm = TRUE), dbl_1e6 = pmax(dbl_1e6, 0),
      two_1e6 = pmax(dbl_1e6, pos_1e6))
bench("pmax.int", dbl_1e3 = pmax.int(dbl_1e3, 0), dbl_1e6 = pmax.int(dbl_1e6, 0))
bench("pmin", scalar = pmin(dbl_1, 0), dbl_1e3 = pmin(dbl_1e3, pos_1e3), zero_1e3 = pmin(dbl_1e3, 0),
      int_1e3 = pmin(int_1e3, 500L), na_1e3 = pmin(na_1e3, 0, na.rm = TRUE), dbl_1e6 = pmin(dbl_1e6, 0))
bench("pmin.int", dbl_1e3 = pmin.int(dbl_1e3, 0), dbl_1e6 = pmin.int(dbl_1e6, 0))
bench("polyroot", setup = { small <- dbl_1e3[1:5]; co <- dbl_1e3[1:50] }, small = polyroot(small), degree_50 = polyroot(co))
bench("pos.to.env", setup = { p <- 1L }, small = pos.to.env(p))
bench("Position", setup = { p <- function(x) x > 99 }, dbl_1e3 = Position(p, dbl_1e3), right_1e3 = Position(p, dbl_1e3, right = TRUE),
      lst_1e3 = Position(is.null, lst_1e3))
bench(".POSIXct", dbl_1e3 = .POSIXct(dbl_1e3, "UTC"), dbl_1e6 = .POSIXct(dbl_1e6))
bench(".POSIXlt", setup = { l <- unclass(as.POSIXlt(time_1e3)) }, time_1e3 = .POSIXlt(l, "UTC"))
bench(".pretty", setup = { r <- range(dbl_1e3) }, small = .pretty(r), bounds = .pretty(r, 10L, bounds = FALSE))
bench("pretty", scalar = pretty(dbl_1), dbl_1e3 = pretty(dbl_1e3), n_1e3 = pretty(dbl_1e3, n = 10), dbl_1e6 = pretty(dbl_1e6))
bench("pretty.default", dbl_1e3 = pretty.default(dbl_1e3))
bench("prettyNum", scalar = prettyNum(dbl_1 * 1e6, big.mark = ","), dbl_1e3 = prettyNum(dbl_1e3 * 1e4, big.mark = ","),
      chr_1e3 = prettyNum(numstr_1e3), plain_1e3 = prettyNum(dbl_1e3))
bench(".Primitive", setup = { s <- "sum" }, small = .Primitive(s))
skip(".primTrace", "changes the session")
skip(".primUntrace", "changes the session")
bench("print", scalar = print(dbl_1), dbl_1e3 = print(dbl_1e3), chr_1e3 = print(chr_1e3), int_1e3 = print(int_1e3),
      lst_10 = print(lst_10), df_10 = print(df_10), df_1e3 = print(df_1e3), fct_1e3 = print(fct_1e3),
      mat_100 = print(mat_100), named_1e3 = print(named_1e3), fun = print(fun))
bench("print.AsIs", setup = { a <- I(dbl_1e3) }, dbl_1e3 = print.AsIs(a))
bench("print.by", setup = { b <- by(df_10$x, df_10$g, sum) }, df_10 = print.by(b))
bench("print.condition", setup = { cond <- simpleError("something went wrong") }, small = print.condition(cond))
bench("print.connection", setup = { con <- stdin() }, small = print.connection(con))
bench("print.data.frame", df_10 = print.data.frame(df_10), df_1e3 = print.data.frame(df_1e3))
bench("print.Date", date_1e3 = print.Date(date_1e3), scalar = print.Date(date_1))
bench("print.default", dbl_1e3 = print.default(dbl_1e3), chr_1e3 = print.default(chr_1e3), lst_10 = print.default(lst_10),
      digits_1e3 = print.default(dbl_1e3, digits = 3))
bench("print.difftime", setup = { d <- as.difftime(dbl_1e3, units = "secs") }, difftime_1e3 = print.difftime(d))
bench("print.Dlist", setup = { d <- structure(stats::setNames(chr_1e3[1:100], sprintf("VAR%03d", 1:100)), class = "Dlist") },
      small = print.Dlist(d))
bench("print.DLLInfo", setup = { dll <- getLoadedDLLs()[["base"]] }, small = print.DLLInfo(dll))
bench("print.DLLInfoList", setup = { dlls <- getLoadedDLLs()["base"] }, small = print.DLLInfoList(dlls))
bench("print.DLLRegisteredRoutines", setup = { r <- getDLLRegisteredRoutines("stats") }, stats = print.DLLRegisteredRoutines(r))
bench("print.eigen", setup = { e <- eigen(spd_100[1:10, 1:10]) }, mat_10 = print.eigen(e))
bench("print.factor", fct_1e3 = print.factor(fct_1e3))
bench("print.function", fun = print.function(fun), closure = print.function(paste))
bench("print.hexmode", setup = { h <- as.hexmode(int_1e3) }, int_1e3 = print.hexmode(h))
skip("print.libraryIQR", "writes files")
bench("print.listof", setup = { l <- structure(list(a = dbl_1e3[1:10], b = chr_1e3[1:10]), class = "listof") }, small = print.listof(l))
bench("print.NativeRoutineList", setup = { r <- getDLLRegisteredRoutines("stats")$.Call }, stats = print.NativeRoutineList(r))
bench("print.noquote", setup = { q <- noquote(chr_1e3) }, chr_1e3 = print.noquote(q))
bench("print.numeric_version", setup = { v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3)) },
      chr_1e3 = print.numeric_version(v))
bench("print.octmode", setup = { o <- as.octmode(int_1e3) }, int_1e3 = print.octmode(o))
skip("print.packageInfo", "writes files")
bench("print.POSIXct", time_1e3 = print.POSIXct(time_1e3), scalar = print.POSIXct(time_1))
bench("print.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = print.POSIXlt(lt))
bench("print.proc_time", setup = {
  pt <- structure(c(user.self = 1.5, sys.self = 0.25, elapsed = 3, user.child = 0, sys.child = 0), class = "proc_time")
}, small = print.proc_time(pt))
bench("print.restart", setup = { r <- withRestarts(computeRestarts()[[1L]], retry = function() NULL) }, small = print.restart(r))
bench("print.rle", setup = { r <- rle(sort(int_1e3)) }, int_1e3 = print.rle(r))
bench("print.simple.list", setup = { l <- as.list(stats::setNames(chr_1e3[1:100], sprintf("k%03d", 1:100))) },
      small = print.simple.list(l))
bench("print.srcfile", setup = { sf <- srcfilecopy("x.R", txt_1e3) }, small = print.srcfile(sf))
bench("print.srcref", setup = { sf <- srcfilecopy("x.R", txt_1e3); sr <- srcref(sf, c(1L, 1L, 3L, 5L)) }, small = print.srcref(sr))
bench("print.summary.table", setup = { s <- summary(table(df_1e3$g, df_1e3$b)) }, small = print.summary.table(s))
bench("print.summary.warnings", setup = {
  w <- structure(stats::setNames(rep(list(quote(f(x))), 10L), paste("problem", c(1:5, 1:5))), class = "warnings")
  sw <- summary(w)
}, small = print.summary.warnings(sw))
bench("print.summaryDefault", setup = { s <- summary(dbl_1e3) }, small = print.summaryDefault(s))
bench("print.table", setup = { t1 <- table(fct_1e3); t2 <- table(df_1e3$g, df_1e3$b) }, fct_1e3 = print.table(t1), two_way = print.table(t2))
bench("print.warnings", setup = {
  w <- structure(stats::setNames(rep(list(quote(f(x))), 10L), paste("problem", 1:10)), class = "warnings")
}, small = print.warnings(w))
bench("prmatrix", mat_10 = prmatrix(mat_10), mat_100 = prmatrix(mat_100))
skip("proc.time", "nondeterministic")
bench("prod", scalar = prod(dbl_1, int_1), dbl_1e3 = prod(dbl_1e3), prob_1e3 = prod(prob_1e3), int_1e3 = prod(int_1e3[1:10]),
      na_1e3 = prod(na_1e3, na.rm = TRUE), prob_1e6 = prod(prob_1e6))
bench("prop.table", setup = { t <- table(df_1e3$g, df_1e3$b) }, table_2 = prop.table(t), margin = prop.table(t, 1),
      mat_100 = prop.table(abs(mat_100)), dbl_1e3 = prop.table(pos_1e3))
bench("proportions", setup = { t <- table(df_1e3$g, df_1e3$b) }, table_2 = proportions(t), margin = proportions(t, 2))
bench("provideDimnames", mat_100 = provideDimnames(mat_100), mat_10 = provideDimnames(mat_10, base = list(letters)))
bench("psigamma", scalar = psigamma(dbl_1), pos_1e3 = psigamma(pos_1e3, 1), pos_1e6 = psigamma(pos_1e6, 0))
bench("pushBack", txt_1e3 = { con <- textConnection(txt_1); pushBack(txt_1e3, con); close(con) })
bench("pushBackLength", setup = { lines <- txt_1e3[1:10] },
      small = { con <- textConnection(txt_1); pushBack(lines, con); n <- pushBackLength(con); close(con); n })
skip("q", "changes the session")
bench("qr", mat_10 = qr(mat_10), mat_100 = qr(mat_100), lapack_100 = qr(mat_100, LAPACK = TRUE), tall_1e3 = qr(cbind(1, dbl_1e3, pos_1e3)))
bench("qr.coef", setup = { q <- qr(mat_100); y <- dbl_1e3[1:100] }, mat_100 = qr.coef(q, y))
bench("qr.default", mat_100 = qr.default(mat_100))
bench("qr.fitted", setup = { q <- qr(mat_100); y <- dbl_1e3[1:100] }, mat_100 = qr.fitted(q, y))
bench("qr.Q", setup = { q <- qr(mat_100) }, mat_100 = qr.Q(q))
bench("qr.qty", setup = { q <- qr(mat_100); y <- dbl_1e3[1:100] }, mat_100 = qr.qty(q, y))
bench("qr.qy", setup = { q <- qr(mat_100); y <- dbl_1e3[1:100] }, mat_100 = qr.qy(q, y))
bench("qr.R", setup = { q <- qr(mat_100) }, mat_100 = qr.R(q))
bench("qr.resid", setup = { q <- qr(cbind(1, dbl_1e3)); y <- pos_1e3 }, tall_1e3 = qr.resid(q, y))
bench("qr.solve", setup = { y <- dbl_1e3[1:100] }, mat_100 = qr.solve(spd_100, y), mat_10 = qr.solve(mat_10))
bench("qr.X", setup = { q <- qr(mat_100) }, mat_100 = qr.X(q))
bench("quarters", date_1e3 = quarters(date_1e3), time_1e3 = quarters(time_1e3))
bench("quarters.Date", date_1e3 = quarters.Date(date_1e3))
bench("quarters.POSIXt", time_1e3 = quarters.POSIXt(time_1e3))
skip("quit", "changes the session")
bench("quote", small = quote(dbl_1 + 1), call = quote(f(dbl_1, chr_1)))
bench("R.home", small = R.home(), component = R.home("bin"))
bench("R.Version", small = R.Version())
bench("R_compiled_by", small = R_compiled_by())
bench("R_system_version", setup = { v <- "4.6.2" }, small = R_system_version(v))
bench("range", scalar = range(dbl_1), dbl_1e3 = range(dbl_1e3), int_1e3 = range(int_1e3), na_1e3 = range(na_1e3, na.rm = TRUE),
      chr_1e3 = range(chr_1e3), finite_1e3 = range(na_1e3, finite = TRUE), dbl_1e6 = range(dbl_1e6))
bench("range.Date", date_1e3 = range.Date(date_1e3), date_1e6 = range.Date(date_1e6))
bench("range.default", dbl_1e3 = range.default(dbl_1e3), dbl_1e6 = range.default(dbl_1e6))
bench("range.POSIXct", time_1e3 = range.POSIXct(time_1e3), time_1e6 = range.POSIXct(time_1e6))
bench(".rangeNum", dbl_1e3 = .rangeNum(dbl_1e3, na.rm = FALSE, finite = FALSE, isNumeric = is.numeric),
      na_1e3 = .rangeNum(na_1e3, na.rm = TRUE, finite = FALSE, isNumeric = is.numeric))
bench("rank", setup = { dbl_1e5 <- dbl_1e6[seq_len(1e5)] }, scalar = rank(dbl_1), dbl_1e3 = rank(dbl_1e3), int_1e3 = rank(int_1e3), first_1e3 = rank(int_1e3, ties.method = "first"),
      min_1e3 = rank(int_1e3, ties.method = "min"), chr_1e3 = rank(chr_1e3), na_1e3 = rank(na_1e3, na.last = "keep"),
      dbl_1e5 = rank(dbl_1e5))
bench("rapply", setup = { nested <- list(a = lst_1e3[1:10], b = list(c = dbl_1e3[1:10], d = chr_1e3[1:5])) },
      lst_1e3 = rapply(lst_1e3, fun, how = "unlist"), list_1e3 = rapply(lst_1e3, fun, how = "list"),
      nested = rapply(nested, fun, classes = "numeric", how = "replace"))
bench("raw", setup = { n <- 1000L }, n_1e3 = raw(n), scalar = raw(int_1))
bench("rawConnection", raw_1e3 = { con <- rawConnection(raw_1e3); close(con) })
bench("rawConnectionValue", setup = { bytes <- raw_1e3 },
      raw_1e3 = { con <- rawConnection(raw(0L), "wb"); writeBin(bytes, con); v <- rawConnectionValue(con); close(con); v })
bench("rawShift", raw_1e3 = rawShift(raw_1e3, 2L), right_1e3 = rawShift(raw_1e3, -3L))
bench("rawToBits", scalar = rawToBits(raw_1e3[1L]), raw_1e3 = rawToBits(raw_1e3))
bench("rawToChar", setup = { r <- charToRaw(paste(txt_1e3, collapse = " ")); s <- charToRaw(txt_1) },
      scalar = rawToChar(s), txt_1e3 = rawToChar(r), multiple = rawToChar(s, multiple = TRUE))
bench("rbind", setup = { r <- dbl_1e3[1:10]; parts <- lapply(1:10, function(i) df_10) },
      small = rbind(r, r), mat_10 = rbind(mat_10, mat_10), dbl_1e3 = rbind(dbl_1e3, pos_1e3), df_10 = rbind(df_10, df_10),
      df_1e3 = rbind(df_1e3, df_1e3), many_df = do.call(rbind, parts), mat_1000 = rbind(mat_1000, mat_1000))
bench("rbind.data.frame", df_10 = rbind.data.frame(df_10, df_10), df_1e3 = rbind.data.frame(df_1e3, df_1e3))
bench("rcond", mat_10 = rcond(mat_10), mat_100 = rcond(mat_100), spd_100 = rcond(spd_100, "I"))
bench("Re", scalar = Re(cpl_1), cpl_1e3 = Re(cpl_1e3), cpl_1e6 = Re(cpl_1e6))
bench("read.dcf", setup = {
  dcf <- c("Package: pkg", "Version: 1.0.0", "Title: A Title", "Imports: stats, utils", "",
           "Package: other", "Version: 2.1", "Depends: R (>= 4.0)")
}, small = { con <- textConnection(dcf); r <- read.dcf(con); close(con); r })
bench("readBin", setup = { bytes <- writeBin(dbl_1e3, raw()) },
      int_1e3 = readBin(raw_1e3, "integer", n = 250L), dbl_1e3 = readBin(bytes, "double", n = 1000L),
      int8_1e3 = readBin(raw_1e3, "integer", n = 1000L, size = 1L, signed = FALSE))
bench("readChar", setup = { r <- charToRaw(paste(txt_1e3, collapse = " ")) },
      small = readChar(r, 100L), many = readChar(r, rep(10L, 100L), useBytes = TRUE))
skip("readline", "interactive")
bench("readLines", txt_1e3 = { con <- textConnection(txt_1e3); r <- readLines(con); close(con); r },
      some = { con <- textConnection(txt_1e3); r <- readLines(con, n = 10L); close(con); r })
bench("readRDS", setup = { bytes <- serialize(df_1e3, NULL, xdr = FALSE) },
      df_1e3 = { con <- rawConnection(bytes); r <- readRDS(con); close(con); r })
skip("readRenviron", "changes the session")
bench("Recall", setup = { f <- function(n) if (n <= 1) 1 else n * Recall(n - 1); k <- 20 }, small = f(k))
bench("Reduce", setup = { f2 <- function(x, y) x + y; small <- lst_1e3[1:10] },
      small = Reduce(`+`, small), lst_1e3 = Reduce(`+`, lst_1e3), fun_1e3 = Reduce(f2, lst_1e3),
      accumulate_1e3 = Reduce(`+`, dbl_1e3, accumulate = TRUE), right_1e3 = Reduce(f2, lst_1e3, right = TRUE),
      init_1e3 = Reduce(f2, dbl_1e3, 0))
bench("reg.finalizer", setup = { e <- new.env(); f <- function(e) NULL }, small = reg.finalizer(e, f))
bench("regexec", chr_1e3 = regexec("id([0-9]+)_([a-z])", chr_1e3), perl_1e3 = regexec("id(\\d+)_(\\w)", chr_1e3, perl = TRUE),
      scalar = regexec("([a-z]+) of", txt_1))
bench("regexpr", scalar = regexpr("[0-9]+", chr_1), chr_1e3 = regexpr("[0-9]+", chr_1e3), perl_1e3 = regexpr("\\d+", chr_1e3, perl = TRUE),
      fixed_1e3 = regexpr("_", chr_1e3, fixed = TRUE), txt_1e3 = regexpr("(price|yield)", txt_1e3),
      chr_1e6 = regexpr("_", chr_1e6, fixed = TRUE))
bench("registerS3method", setup = {
  gen <- function(x, ...) UseMethod("gen")
  method <- function(x, ...) 1
}, small = registerS3method("gen", "foo", method, envir = environment()))
skip("registerS3methods", "internal")
bench("regmatches", setup = { m <- regexpr("[0-9]+", chr_1e3); g <- gregexpr("[a-z]+", txt_1e3) },
      chr_1e3 = regmatches(chr_1e3, m), gregexpr_1e3 = regmatches(txt_1e3, g),
      invert_1e3 = regmatches(chr_1e3, m, invert = TRUE))
bench("regmatches<-", setup = { y <- chr_1e3; m <- regexpr("[0-9]+", y); r <- rep("N", length(y)) },
      chr_1e3 = { z <- y; regmatches(z, m) <- r; z })
bench("remove", setup = { e <- new.env() }, small = { assign("a", 1, envir = e); remove("a", envir = e) },
      list = { assign("a", 1, envir = e); assign("b", 2, envir = e); remove(list = c("a", "b"), envir = e) })
bench("removeTaskCallback", setup = { f <- function(...) TRUE }, small = { id <- addTaskCallback(f); removeTaskCallback(id) },
      absent = removeTaskCallback(int_1 + 1000L))
bench("rep", scalar = rep(dbl_1, int_1), dbl_1e3 = rep(dbl_1e3, 2L), each_1e3 = rep(dbl_1e3, each = 2L),
      times_1e3 = rep(int_1e3[1:10], int_1e3[1:10]), chr_1e3 = rep(chr_1e3, 3L), len_1e3 = rep(dbl_1e3, length.out = 1500L),
      lst_1e3 = rep(lst_1e3, 2L), fill_1e3 = rep(dbl_1, 1000L), dbl_1e6 = rep(dbl_1e6, 2L), big_1e3 = rep(dbl_1e3, 1000L))
bench("rep.Date", date_1e3 = rep(date_1e3, 2L))
bench("rep.difftime", setup = { d <- as.difftime(dbl_1e3, units = "secs") }, difftime_1e3 = rep.difftime(d, 2L))
bench("rep.factor", fct_1e3 = rep(fct_1e3, 2L), each_1e3 = rep(fct_1e3, each = 2L))
bench("rep.int", scalar = rep.int(dbl_1, int_1), dbl_1e3 = rep.int(dbl_1e3, 2L), fill_1e3 = rep.int(dbl_1, 1000L),
      dbl_1e6 = rep.int(dbl_1e6, 2L))
bench("rep.numeric_version", setup = { v <- numeric_version(sprintf("%d.%d", int_1e3 %% 10L, int_1e3)) },
      chr_1e3 = rep.numeric_version(v, 2L))
bench("rep.POSIXct", time_1e3 = rep(time_1e3, 2L))
bench("rep.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = rep.POSIXlt(lt, 2L))
bench("rep_len", scalar = rep_len(dbl_1, int_1), dbl_1e3 = rep_len(dbl_1e3, 1500L), short_1e3 = rep_len(dbl_1e3[1:3], 1000L),
      chr_1e3 = rep_len(chr_1e3, 2000L), dbl_1e6 = rep_len(dbl_1e6, 1500000L))
skip("repeat", "language construct")
bench("replace", setup = { idx <- int_1e3[1:100]; big <- int_1e6[1:1000] },
      dbl_1e3 = replace(dbl_1e3, idx, 0), lgl_1e3 = replace(na_1e3, is.na(na_1e3), 0), dbl_1e6 = replace(dbl_1e6, big, 0))
bench("replicate", setup = { n <- 100L; small <- dbl_1e3[1:10] },
      dbl_100 = replicate(n, mean(small)), list_100 = replicate(n, small, simplify = FALSE),
      runif_100 = replicate(n, runif(3)))
skip("require", "changes the session")
bench("requireNamespace", setup = { p <- "stats" }, loaded = requireNamespace(p, quietly = TRUE))
bench("restartDescription", setup = { r <- withRestarts(computeRestarts()[[1L]], retry = function() NULL) },
      small = restartDescription(r))
bench("restartFormals", setup = { r <- withRestarts(computeRestarts()[[1L]], retry = function(x, y) NULL) },
      small = restartFormals(r))
bench("retracemem", small = retracemem(dbl_1e3))
