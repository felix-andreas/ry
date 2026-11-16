bench("col", mat_100 = col(mat_100), mat_1000 = col(mat_1000))
bench(".colMeans", mat_100 = .colMeans(mat_100, 100L, 100L), mat_1000 = .colMeans(mat_1000, 1000L, 1000L))
bench("colMeans", mat_10 = colMeans(mat_10), mat_100 = colMeans(mat_100), mat_1000 = colMeans(mat_1000),
      imat_100 = colMeans(imat_100), df_1e3 = colMeans(df_1e3[c("x", "y")]))
bench("colnames",
      setup = { named <- mat_100; dimnames(named) <- list(NULL, sprintf("c%03d", 1:100)) },
      mat_100 = colnames(named), unnamed = colnames(mat_100), df_10 = colnames(df_10), df_1e3 = colnames(df_1e3))
bench("colnames<-",
      setup = { y <- mat_100; nm <- sprintf("c%03d", 1:100); d <- df_1e3; dn <- paste0("v", 1:6) },
      mat_100 = colnames(y) <- nm, df_1e3 = colnames(d) <- dn)
bench(".colSums", mat_100 = .colSums(mat_100, 100L, 100L), mat_1000 = .colSums(mat_1000, 1000L, 1000L))
bench("colSums", mat_10 = colSums(mat_10), mat_100 = colSums(mat_100), mat_1000 = colSums(mat_1000),
      imat_100 = colSums(imat_100), df_1e3 = colSums(df_1e3[c("x", "y")]))
bench("commandArgs", setup = { trailing <- TRUE }, scalar = commandArgs(trailing))
bench("comment", setup = { y <- dbl_1e3; comment(y) <- "a note" }, scalar = comment(y))
bench("comment<-", setup = { y <- dbl_1e3; note <- "a note" }, dbl_1e3 = comment(y) <- note)
bench("complex", scalar = complex(real = dbl_1, imaginary = dbl_1),
      dbl_1e3 = complex(real = dbl_1e3, imaginary = dbl_1e3),
      dbl_1e6 = complex(real = dbl_1e6, imaginary = dbl_1e6),
      modulus_1e3 = complex(modulus = pos_1e3, argument = dbl_1e3))
bench("computeRestarts", scalar = computeRestarts(),
      nested = withRestarts(computeRestarts(), first = function() NULL, second = function(x) x))
bench("conditionCall", setup = { condition <- simpleError("bad input", call = quote(f(x, y))) },
      error = conditionCall(condition))
bench("conditionCall.condition", setup = { condition <- simpleCondition("bad input", call = quote(f(x))) },
      condition = conditionCall.condition(condition))
bench("conditionMessage", setup = {
  error <- simpleError("bad input", call = quote(f(x)))
  warning <- simpleWarning("careful")
}, error = conditionMessage(error), warning = conditionMessage(warning))
bench("conditionMessage.condition", setup = { condition <- simpleCondition("bad input") },
      condition = conditionMessage.condition(condition))
bench("conflictRules", setup = { package <- "nopkg" }, scalar = conflictRules(package))
bench("conflicts", setup = { detail <- FALSE }, scalar = conflicts(detail = detail))
bench("Conj", scalar = Conj(cpl_1), cpl_1e3 = Conj(cpl_1e3), cpl_1e6 = Conj(cpl_1e6), dbl_1e3 = Conj(dbl_1e3))
skip("contributors", "reads files")
bench("cos", scalar = cos(dbl_1), dbl_1e3 = cos(dbl_1e3), dbl_1e6 = cos(dbl_1e6), cpl_1e3 = cos(cpl_1e3))
bench("cosh", scalar = cosh(dbl_1), prob_1e3 = cosh(prob_1e3), prob_1e6 = cosh(prob_1e6))
bench("cospi", scalar = cospi(dbl_1), dbl_1e3 = cospi(dbl_1e3), dbl_1e6 = cospi(dbl_1e6))
bench("crossprod", mat_10 = crossprod(mat_10), mat_100 = crossprod(mat_100), two_mat_100 = crossprod(mat_100, spd_100),
      dbl_1e3 = crossprod(dbl_1e3), dbl_1e6 = crossprod(dbl_1e6, dbl_1e6))
skip("Cstack_info", "nondeterministic")
bench("cummax", dbl_1e3 = cummax(dbl_1e3), dbl_1e6 = cummax(dbl_1e6), int_1e3 = cummax(int_1e3), int_1e6 = cummax(int_1e6))
bench("cummin", dbl_1e3 = cummin(dbl_1e3), dbl_1e6 = cummin(dbl_1e6), int_1e3 = cummin(int_1e3), int_1e6 = cummin(int_1e6))
bench("cumprod", prob_1e3 = cumprod(prob_1e3), prob_1e6 = cumprod(prob_1e6), cpl_1e3 = cumprod(cpl_1e3))
bench("cumsum", dbl_1e3 = cumsum(dbl_1e3), dbl_1e6 = cumsum(dbl_1e6), int_1e3 = cumsum(int_1e3),
      int_1e6 = cumsum(int_1e6), lgl_1e6 = cumsum(lgl_1e6), na_1e3 = cumsum(na_1e3))
skip("curlGetHeaders", "network")
bench("cut", setup = { breaks <- seq(-100, 100, by = 10) },
      dbl_1e3 = cut(dbl_1e3, 10), breaks_1e3 = cut(dbl_1e3, breaks), dbl_1e6 = cut(dbl_1e6, breaks),
      int_1e3 = cut(int_1e3, c(0, 250, 500, 750, 1000), labels = FALSE))
bench("cut.Date", setup = { by <- "month" }, month_1e3 = cut.Date(date_1e3, by), year_1e3 = cut.Date(date_1e3, "year"),
      week_1e3 = cut.Date(date_1e3, "week"))
bench("cut.default", setup = { breaks <- seq(-100, 100, by = 10) },
      dbl_1e3 = cut.default(dbl_1e3, breaks), dbl_1e6 = cut.default(dbl_1e6, breaks, labels = FALSE))
bench("cut.POSIXt", setup = { by <- "day" }, day_1e3 = cut.POSIXt(time_1e3, by), month_1e3 = cut.POSIXt(time_1e3, "month"))
bench("data.class", dbl_1e3 = data.class(dbl_1e3), df_10 = data.class(df_10), fct_1e3 = data.class(fct_1e3), mat_10 = data.class(mat_10))
bench("data.frame", small = data.frame(a = dbl_1, b = chr_1), df_10 = data.frame(df_10, z = 1:10),
      dbl_1e3 = data.frame(x = dbl_1e3, y = int_1e3, s = chr_1e3),
      dbl_1e6 = data.frame(x = dbl_1e6, y = int_1e6, s = chr_1e6),
      factors_1e3 = data.frame(x = dbl_1e3, s = chr_1e3, stringsAsFactors = TRUE),
      mat_100 = data.frame(mat_100))
bench("data.matrix", df_10 = data.matrix(df_10), df_1e3 = data.matrix(df_1e3), df_1e6 = data.matrix(df_1e6[c("id", "x", "y", "g")]))
bench(".Date", scalar = .Date(dbl_1), int_1e3 = .Date(int_1e3), dbl_1e6 = .Date(dbl_1e6))
skip("date", "nondeterministic")
skip("debug", "changes the session")
bench("debuggingState", scalar = debuggingState())
skip("debugonce", "changes the session")
bench("declare", scalar = declare(dbl_1))
bench(".decode_numeric_version",
      setup = { encoded <- .encode_numeric_version(numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))) },
      ver_1e3 = .decode_numeric_version(encoded))
skip("default.stringsAsFactors", "defunct")
skip(".Defunct", "defunct")
bench("delayedAssign", setup = { e <- new.env() }, scalar = delayedAssign("z", dbl_1 + 1, assign.env = e))
bench("deparse", setup = { call <- quote(f(x, y = 2, z = g(h + 1))) },
      call = deparse(call), fun = deparse(fun), formula = deparse(formula_1), dbl_1e3 = deparse(dbl_1e3),
      chr_1e3 = deparse(chr_1e3), df_10 = deparse(df_10), lst_10 = deparse(lst_10), mat_100 = deparse(mat_100))
bench("deparse1", setup = { call <- quote(f(x, y = 2, z = g(h + 1))) },
      call = deparse1(call), fun = deparse1(fun), dbl_1e3 = deparse1(dbl_1e3), df_10 = deparse1(df_10))
bench(".deparseOpts", setup = { opts <- c("keepNA", "keepInteger", "niceNames", "showAttributes") },
      scalar = .deparseOpts(opts), all = .deparseOpts(c(opts, "useSource", "warnIncomplete")))
bench(".Deprecated", setup = { new <- "newer" }, caught = tryCatch(.Deprecated(new), warning = identity))
bench("det", mat_10 = det(mat_10), mat_100 = det(mat_100), spd_100 = det(spd_100))
skip(".detach", "changes the session")
skip("detach", "changes the session")
bench("determinant", mat_10 = determinant(mat_10), mat_100 = determinant(mat_100), spd_100 = determinant(spd_100, logarithm = FALSE))
bench("determinant.matrix", mat_100 = determinant.matrix(mat_100), spd_100 = determinant.matrix(spd_100))
skip("dget", "reads files")
bench("diag", setup = { n <- 100L; small <- dbl_1e3[1:100] }, mat_100 = diag(mat_100), mat_1000 = diag(mat_1000),
      identity_100 = diag(n), dbl_100 = diag(small), dbl_1e3 = diag(dbl_1e3))
bench("diag<-", setup = { y <- mat_100; z <- mat_1000; values <- dbl_1e3[1:100] },
      mat_100 = diag(y) <- 0, values_100 = diag(y) <- values, mat_1000 = diag(z) <- 1)
bench("diff", dbl_1e3 = diff(dbl_1e3), dbl_1e6 = diff(dbl_1e6), int_1e3 = diff(int_1e3), int_1e6 = diff(int_1e6),
      lag_1e3 = diff(dbl_1e3, lag = 2L, differences = 2L), mat_100 = diff(mat_100), ts_1e3 = diff(ts_1e3))
bench("diff.Date", date_1e3 = diff.Date(date_1e3), date_1e6 = diff.Date(date_1e6))
bench("diff.default", dbl_1e3 = diff.default(dbl_1e3), dbl_1e6 = diff.default(dbl_1e6))
bench("diff.difftime", setup = { span <- as.difftime(sorted_1e3, units = "secs") }, dbl_1e3 = diff.difftime(span))
bench("diff.POSIXt", time_1e3 = diff.POSIXt(time_1e3), time_1e6 = diff.POSIXt(time_1e6))
bench(".difftime", setup = { units <- "secs" }, dbl_1e3 = .difftime(dbl_1e3, units), dbl_1e6 = .difftime(dbl_1e6, units))
bench("difftime", setup = { units <- "days" }, scalar = difftime(time_1, time_1e3[1L]),
      time_1e3 = difftime(time_1e3, time_1, units = units), auto_1e3 = difftime(time_1e3, time_1),
      date_1e3 = difftime(date_1e3, date_1, units = units), time_1e6 = difftime(time_1e6, time_1, units = units))
bench("digamma", scalar = digamma(dbl_1), pos_1e3 = digamma(pos_1e3), pos_1e6 = digamma(pos_1e6))
bench("dim", mat_100 = dim(mat_100), df_10 = dim(df_10), df_1e3 = dim(df_1e3), dbl_1e3 = dim(dbl_1e3))
bench("dim.data.frame", df_10 = dim.data.frame(df_10), df_1e3 = dim.data.frame(df_1e3))
bench("dim<-", setup = { y <- dbl_1e3; shape <- c(10L, 100L) }, dbl_1e3 = dim(y) <- shape)
bench("dimnames", setup = { named <- mat_100; dimnames(named) <- list(sprintf("r%03d", 1:100), sprintf("c%03d", 1:100)) },
      mat_100 = dimnames(named), unnamed = dimnames(mat_100), df_10 = dimnames(df_10), df_1e3 = dimnames(df_1e3))
bench("dimnames.data.frame", df_10 = dimnames.data.frame(df_10), df_1e3 = dimnames.data.frame(df_1e3))
bench("dimnames<-", setup = {
  y <- mat_100
  names <- list(sprintf("r%03d", 1:100), sprintf("c%03d", 1:100))
  d <- df_10
  df_names <- list(letters[1:10], c("a", "b", "c", "d"))
}, mat_100 = dimnames(y) <- names, df_10 = dimnames(d) <- df_names)
bench("dimnames<-.data.frame", setup = {
  d <- df_1e3
  names <- list(as.character(seq_len(1000L)), c("a", "b", "c", "d", "e", "f"))
}, df_1e3 = `dimnames<-.data.frame`(d, names))
skip("dir", "reads files")
skip("dir.create", "writes files")
skip("dir.exists", "reads files")
bench("dirname", setup = { paths <- file.path("root", "sub", chr_1e3, "file.R"); path <- "/usr/lib/R/library" },
      scalar = dirname(path), chr_1e3 = dirname(paths))
bench("do.call", setup = {
  args <- list(dbl_1e3, na.rm = TRUE)
  pieces <- split(df_1e3, df_1e3$g)
  strings <- as.list(chr_1e3[1:10])
}, small = do.call(sum, args), by_name = do.call("paste", strings), lst_1e3 = do.call(c, lst_1e3),
      rbind_df = do.call(rbind, pieces), fun = do.call(fun, list(dbl_1e3)))
bench("dontCheck", scalar = dontCheck(dbl_1))
bench(".doSortWrap", setup = { x <- sorted_1e3 }, dbl_1e3 = .doSortWrap(x, FALSE, TRUE))
bench(".doTrace", setup = { value <- dbl_1 }, scalar = .doTrace(value))
bench("double", setup = { n <- 1000L; m <- 1000000L }, scalar = double(int_1), len_1e3 = double(n), len_1e6 = double(m))
bench(".doWrap", setup = { x <- sorted_1e3 }, dbl_1e3 = .doWrap(x, FALSE, TRUE))
bench("dput", scalar = dput(dbl_1), df_10 = dput(df_10), lst_10 = dput(lst_10), fun = dput(fun), dbl_1e3 = dput(dbl_1e3))
bench("dQuote", scalar = dQuote(chr_1), chr_1e3 = dQuote(chr_1e3), plain_1e3 = dQuote(chr_1e3, FALSE))
bench("drop", setup = { column <- mat_100[, 1L, drop = FALSE]; arr <- array(dbl_1e3, c(10L, 1L, 100L)) },
      column = drop(column), array_1e3 = drop(arr), mat_100 = drop(mat_100))
bench("droplevels", setup = { part <- fct_1e3[fct_1e3 %in% c("a", "b", "c")]; d <- df_1e3[df_1e3$g %in% c("a", "b"), ] },
      fct_1e3 = droplevels(part), df_1e3 = droplevels(d))
bench("droplevels.data.frame", setup = { d <- df_1e3[df_1e3$g %in% c("a", "b"), ] }, df_1e3 = droplevels.data.frame(d))
bench("droplevels.factor", setup = { part <- fct_1e3[fct_1e3 %in% c("a", "b", "c")] },
      fct_1e3 = droplevels.factor(part), fct_1e6 = droplevels.factor(fct_1e6, exclude = "z"))
bench("dump", setup = { e <- new.env(); e$x <- df_10; e$f <- fun }, df_10 = dump(c("x", "f"), file = "", envir = e))
bench("duplicated", dbl_1e3 = duplicated(dbl_1e3), int_1e3 = duplicated(int_1e3), int_1e6 = duplicated(int_1e6),
      chr_1e3 = duplicated(chr_1e3), chr_1e6 = duplicated(chr_1e6), fct_1e3 = duplicated(fct_1e3),
      lst_10 = duplicated(lst_10), df_1e3 = duplicated(df_1e3))
bench("duplicated.array", setup = { arr <- array(int_1e3 %% 3L, c(100L, 5L, 2L)) }, array_1e3 = duplicated.array(arr))
bench("duplicated.data.frame", setup = { d <- df_1e3[c("g", "b")] }, df_10 = duplicated.data.frame(df_10),
      df_1e3 = duplicated.data.frame(df_1e3), few_1e3 = duplicated.data.frame(d))
bench("duplicated.default", dbl_1e3 = duplicated.default(dbl_1e3), int_1e6 = duplicated.default(int_1e6))
bench("duplicated.matrix", setup = { m <- rbind(imat_100, imat_100) }, imat_100 = duplicated.matrix(m), mat_100 = duplicated.matrix(mat_100))
bench("duplicated.numeric_version", setup = { v <- numeric_version(sprintf("%d.%d", int_1e3 %% 10L, int_1e3 %% 7L)) },
      ver_1e3 = duplicated.numeric_version(v))
bench("duplicated.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = duplicated.POSIXlt(lt))
bench("duplicated.warnings", setup = {
  w <- structure(rep(list(quote(f(x)), quote(g(y))), 50L), names = rep(c("first", "second", "third", "fourth"), 25L), class = "warnings")
}, small = duplicated.warnings(w))
skip("dyn.load", "reads files")
skip("dyn.unload", "changes the session")
bench("dynGet", setup = { name <- "calls"; missing_name <- "not_a_variable" },
      found = dynGet(name), missing = dynGet(missing_name, ifnotfound = NA))
bench(".dynLibs", scalar = .dynLibs())
bench("eapply", setup = { f <- function(x) x + 1L }, env_1e3 = eapply(env_1e3, f))
bench("eigen", mat_10 = eigen(mat_10), spd_100 = eigen(spd_100, symmetric = TRUE),
      values_100 = eigen(spd_100, symmetric = TRUE, only.values = TRUE), mat_100 = eigen(mat_100))
bench("...elt", setup = { second <- function(...) ...elt(2L) }, scalar = second(dbl_1, dbl_1e3))
bench("emptyenv", scalar = emptyenv())
bench("enc2native", scalar = enc2native(chr_1), chr_1e3 = enc2native(chr_1e3), chr_1e6 = enc2native(chr_1e6))
bench("enc2utf8", scalar = enc2utf8(chr_1), chr_1e3 = enc2utf8(chr_1e3), chr_1e6 = enc2utf8(chr_1e6))
bench(".encode_numeric_version", setup = { v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3)) },
      ver_1e3 = .encode_numeric_version(v))
bench("encodeString", scalar = encodeString(txt_1), chr_1e3 = encodeString(chr_1e3, quote = "\""),
      txt_1e3 = encodeString(txt_1e3, width = NA), chr_1e5 = encodeString(chr_1e6[1:100000]))
bench("Encoding", scalar = Encoding(chr_1), chr_1e3 = Encoding(chr_1e3), chr_1e6 = Encoding(chr_1e6))
bench("Encoding<-", setup = { y <- chr_1e3; enc <- "UTF-8" }, chr_1e3 = Encoding(y) <- enc)
bench("endsWith", setup = { suffix <- "_a" }, scalar = endsWith(chr_1, suffix), chr_1e3 = endsWith(chr_1e3, suffix),
      chr_1e6 = endsWith(chr_1e6, suffix))
bench("enquote", setup = { call <- quote(f(x, y)) }, scalar = enquote(call))
bench("env.profile", env_1e3 = env.profile(env_1e3))
bench("environment", fun = environment(fun), current = environment(), null = environment(NULL))
bench("environment<-", setup = { f <- fun; e <- new.env() }, fun = environment(f) <- e)
bench("environmentIsLocked", env_1e3 = environmentIsLocked(env_1e3))
bench("environmentName", setup = { global <- globalenv(); namespace <- asNamespace("stats") },
      global = environmentName(global), namespace = environmentName(namespace), env_1e3 = environmentName(env_1e3))
bench("errorCondition", setup = { msg <- "bad input" }, scalar = errorCondition(msg, class = "custom_error", data = dbl_1))
bench("eval", setup = { expr <- quote(x + y); data <- list(x = dbl_1e3, y = dbl_1e3); e <- new.env(); e$x <- 1; e$y <- 2 },
      env = eval(expr, e), lst_1e3 = eval(expr, data), df_1e3 = eval(expr, df_1e3), formula = eval(formula_1))
bench("eval.parent", setup = { expr <- quote(dbl_1 + 1); helper <- function() eval.parent(expr) },
      scalar = eval.parent(expr, 0L), caller = helper())
bench("evalq", scalar = evalq(dbl_1 + 1), env_1e3 = evalq(v0001 + v1000, env_1e3))
bench("Exec", setup = { plus <- function(x) Exec(quote(x + 1)) }, scalar = plus(dbl_1), dbl_1e3 = plus(dbl_1e3))
bench("exists", setup = { name <- "v0500"; base_name <- "sum"; missing_name <- "not_a_variable" },
      env_1e3 = exists(name, envir = env_1e3, inherits = FALSE), search = exists(base_name),
      missing = exists(missing_name), mode = exists(base_name, mode = "function"))
bench("exp", scalar = exp(dbl_1), dbl_1e3 = exp(dbl_1e3), dbl_1e6 = exp(dbl_1e6), cpl_1e3 = exp(cpl_1e3))
bench("expand.grid", setup = { a <- 1:100; b <- letters; c <- c(TRUE, FALSE) },
      small = expand.grid(a = a[1:5], b = b[1:5]), grid_5200 = expand.grid(a = a, b = b, c = c),
      plain_5200 = expand.grid(a, b, c, KEEP.OUT.ATTRS = FALSE, stringsAsFactors = FALSE))
bench(".expand_R_libs_env_var", setup = { spec <- "/opt/R/%V/library:%U" }, scalar = .expand_R_libs_env_var(spec))
bench("expm1", scalar = expm1(dbl_1), prob_1e3 = expm1(prob_1e3), prob_1e6 = expm1(prob_1e6))
bench("expression", scalar = expression(dbl_1 + 1), several = expression(a, b + c, f(dbl_1)))
bench(".External", setup = { symbol <- stats:::C_termsform }, formula = .External(symbol, formula_1, NULL, NULL, FALSE, FALSE))
skip(".External.graphics", "graphics devices")
bench(".External2", setup = {
  frame <- stats::model.frame(y ~ x + g, model_1e3)
  stats::contrasts(frame$g) <- "contr.treatment"
  terms <- attr(frame, "terms")
  symbol <- stats:::C_modelmatrix
}, model_1e3 = .External2(symbol, terms, frame))
bench("extSoftVersion", scalar = extSoftVersion())
bench("factor", setup = { lev <- sort(unique(chr_1e3)); small <- c("b", "a", "c", "a") },
      small = factor(small), chr_1e3 = factor(chr_1e3), chr_1e5 = factor(chr_1e6[1:100000]), int_1e3 = factor(int_1e3),
      int_1e6 = factor(int_1e6), levels_1e3 = factor(chr_1e3, levels = lev), fct_1e3 = factor(fct_1e3),
      labels_1e3 = factor(lgl_1e3, labels = c("no", "yes")))
bench("factorial", scalar = factorial(int_1), pos_1e3 = factorial(pos_1e3), pos_1e6 = factorial(pos_1e6))
skip("fifo", "writes files")
skip("file", "reads files")
skip("file.access", "reads files")
skip("file.append", "writes files")
skip("file.choose", "interactive")
skip("file.copy", "writes files")
skip("file.create", "writes files")
skip("file.exists", "reads files")
skip("file.info", "reads files")
skip("file.link", "writes files")
skip("file.mode", "reads files")
skip("file.mtime", "reads files")
bench("file.path", setup = { root <- "/home/user/project" }, scalar = file.path(root, "R", chr_1),
      chr_1e3 = file.path(root, chr_1e3, "data.csv"), chr_1e6 = file.path(root, chr_1e6))
skip("file.remove", "writes files")
skip("file.rename", "writes files")
skip("file.show", "interactive")
skip("file.size", "reads files")
skip("file.symlink", "writes files")
bench("Filter", setup = { positive <- function(x) x > 0 }, lst_1e3 = Filter(positive, lst_1e3),
      dbl_1e3 = Filter(positive, dbl_1e3), lst_10 = Filter(is.numeric, lst_10))
bench("Find", setup = { big <- function(x) x > 99.9 }, lst_1e3 = Find(big, lst_1e3), dbl_1e3 = Find(big, dbl_1e3))
skip("find.package", "reads files")
bench("findInterval", setup = { breaks <- seq(-100, 100, length.out = 101L) }, scalar = findInterval(dbl_1, breaks),
      dbl_1e3 = findInterval(dbl_1e3, breaks), dbl_1e6 = findInterval(dbl_1e6, breaks),
      sorted_1e3 = findInterval(sorted_1e3, sorted_1e3[c(TRUE, FALSE)]))
bench("findPackageEnv", setup = { info <- "package:base" }, scalar = findPackageEnv(info))
bench("findRestart", setup = { name <- "abort"; missing_name <- "not_a_restart" },
      found = findRestart(name), missing = findRestart(missing_name))
skip(".First.sys", "changes the session")
bench("floor", scalar = floor(dbl_1), dbl_1e3 = floor(dbl_1e3), dbl_1e6 = floor(dbl_1e6), int_1e3 = floor(int_1e3))
bench("flush", setup = { con <- stdout() }, stdout = flush(con))
bench("flush.connection", setup = { con <- stdout() }, stdout = flush.connection(con))
skip("for", "language construct")
bench("force", scalar = force(dbl_1), dbl_1e3 = force(dbl_1e3))
bench("forceAndCall", setup = { f <- function(x, y) x + y }, scalar = forceAndCall(1, fun, dbl_1),
      two = forceAndCall(2, f, dbl_1e3, dbl_1e3))
bench("formals", setup = { f <- paste }, fun = formals(fun), closure = formals(f), lm = formals(stats::lm))
bench("formals<-", setup = { f <- fun; arguments <- alist(x = , y = 2) }, fun = formals(f) <- arguments)
bench("format", scalar = format(dbl_1), dbl_1e3 = format(dbl_1e3), dbl_1e5 = format(dbl_1e6[1:100000]),
      nsmall_1e3 = format(dbl_1e3, nsmall = 2), int_1e3 = format(int_1e3), int_1e6 = format(int_1e6),
      big_1e3 = format(pos_1e3 * 1e4, big.mark = ","), chr_1e3 = format(chr_1e3, width = 12),
      lgl_1e3 = format(lgl_1e3), date_1e3 = format(date_1e3), df_10 = format(df_10), lst_10 = format(lst_10),
      na_1e3 = format(na_1e3))
bench("format.AsIs", setup = { x <- I(lst_10); y <- I(dbl_1e3) }, lst_10 = format.AsIs(x), dbl_1e3 = format.AsIs(y))
bench("format.data.frame", df_10 = format.data.frame(df_10), df_1e3 = format.data.frame(df_1e3))
bench("format.Date", setup = { fmt <- "%d/%m/%Y" }, scalar = format.Date(date_1),
      date_1e3 = format.Date(date_1e3), fmt_1e3 = format.Date(date_1e3, fmt), date_1e5 = format.Date(date_1e6[1:100000]))
bench("format.default", scalar = format.default(dbl_1), dbl_1e3 = format.default(dbl_1e3, digits = 3), chr_1e3 = format.default(chr_1e3))
bench("format.difftime", setup = { span <- as.difftime(dbl_1e3, units = "mins") }, dbl_1e3 = format.difftime(span))
bench("format.factor", fct_1e3 = format.factor(fct_1e3), fct_1e6 = format.factor(fct_1e6))
bench("format.hexmode", setup = { x <- as.hexmode(int_1e3) }, int_1e3 = format.hexmode(x))
bench("format.info", scalar = format.info(dbl_1), dbl_1e3 = format.info(dbl_1e3), int_1e3 = format.info(int_1e3),
      dbl_1e6 = format.info(dbl_1e6))
bench("format.libraryIQR", setup = {
  db <- cbind(Package = sprintf("pkg%02d", 1:20), LibPath = rep(c("/lib/one", "/lib/two"), each = 10L),
              Title = paste("Tools for", rep(c("bond pricing", "yield curves"), 10L)))
  x <- structure(list(header = NULL, results = db, footer = NULL), class = "libraryIQR")
}, small = format.libraryIQR(x))
bench("format.numeric_version", setup = { v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3)) },
      ver_1e3 = format.numeric_version(v))
bench("format.octmode", setup = { x <- as.octmode(int_1e3) }, int_1e3 = format.octmode(x))
bench("format.packageInfo", setup = {
  x <- structure(list(name = "pkg", path = "/lib/pkg",
                      info = list(c("Package: pkg", "Title: Bond Tools", "Version: 1.0"),
                                  cbind(c("price", "yield"), c("Price a bond", "Compute a yield")), NULL)),
                 class = "packageInfo")
}, small = format.packageInfo(x))
bench("format.POSIXct", setup = { fmt <- "%Y-%m-%d %H:%M" }, scalar = format.POSIXct(time_1),
      time_1e3 = format.POSIXct(time_1e3), fmt_1e3 = format.POSIXct(time_1e3, fmt), time_1e5 = format.POSIXct(time_1e6[1:100000]))
bench("format.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3); one <- as.POSIXlt(time_1) }, scalar = format.POSIXlt(one),
      time_1e3 = format.POSIXlt(lt), fmt_1e3 = format.POSIXlt(lt, "%H:%M:%S"))
bench("format.pval", scalar = format.pval(dbl_1), prob_1e3 = format.pval(prob_1e3), digits_1e3 = format.pval(prob_1e3 / 1e4, digits = 3))
bench("format.summaryDefault", setup = { s <- summary(dbl_1e3) }, small = format.summaryDefault(s))
bench(".format.zeros", setup = { x <- format(c(0, dbl_1e3, 0)) }, dbl_1e3 = .format.zeros(x, "."))
bench("formatC", scalar = formatC(dbl_1, format = "f", digits = 2), dbl_1e3 = formatC(dbl_1e3, format = "f", digits = 2),
      dbl_1e6 = formatC(dbl_1e6, format = "f", digits = 2), int_1e3 = formatC(int_1e3, width = 8, flag = "0"),
      big_1e3 = formatC(pos_1e3 * 1e4, format = "d", big.mark = ","), exp_1e3 = formatC(dbl_1e3, format = "e", digits = 3),
      chr_1e3 = formatC(chr_1e3, width = -12))
bench("formatDL", setup = { terms <- chr_1e3[1:100]; descriptions <- txt_1e3[1:100] },
      small = formatDL(terms, descriptions), list = formatDL(terms, descriptions, style = "list"))
bench(".formula2varlist", setup = { rhs <- ~ x + g }, model_1e3 = .formula2varlist(rhs, model_1e3))
bench(".Fortran", setup = { x <- mat_100; p <- 100L; tol <- 1e-7; routine <- .F_dqrdc2 },
      mat_100 = .Fortran(routine, qr = x, p, p, p, tol, rank = integer(1L), qraux = double(p), pivot = seq_len(p), double(2L * p)))
bench("forwardsolve", setup = { lower <- t(chol(spd_100)); b <- dbl_1e3[1:100]; bm <- mat_100 },
      vector = forwardsolve(lower, b), mat_100 = forwardsolve(lower, bm))
skip("function", "language construct")
bench("gamma", scalar = gamma(dbl_1), pos_1e3 = gamma(pos_1e3), pos_1e6 = gamma(pos_1e6))
skip("gc", "nondeterministic")
skip("gc.time", "nondeterministic")
skip("gcinfo", "changes the session")
skip("gctorture", "changes the session")
skip("gctorture2", "changes the session")
bench("get", setup = { name <- "v0500"; base_name <- "sum" },
      env_1e3 = get(name, envir = env_1e3), search = get(base_name), mode = get(base_name, mode = "function"),
      inherits = get(name, envir = env_1e3, inherits = FALSE))
bench("get0", setup = { name <- "v0500"; missing_name <- "not_a_variable" },
      env_1e3 = get0(name, envir = env_1e3), missing = get0(missing_name, ifnotfound = NA))
bench("getAllConnections", scalar = getAllConnections())
bench("getCallingDLL", setup = { f <- stats::lm }, scalar = getCallingDLL(f))
bench("getCallingDLLe", setup = { e <- asNamespace("stats") }, scalar = getCallingDLLe(e))
bench("getConnection", setup = { what <- 1L }, scalar = getConnection(what))
bench("getDLLRegisteredRoutines", setup = { dll <- "stats" }, scalar = getDLLRegisteredRoutines(dll))
bench("getDLLRegisteredRoutines.character", setup = { dll <- "stats" }, scalar = getDLLRegisteredRoutines.character(dll))
bench("getDLLRegisteredRoutines.DLLInfo", setup = { dll <- getLoadedDLLs()[["stats"]] }, scalar = getDLLRegisteredRoutines.DLLInfo(dll))
bench("getElement", setup = { name <- "b" }, lst_10 = getElement(lst_10, name), df_10 = getElement(df_10, "x"),
      named_1e3 = getElement(named_1e3, "n0000500"))
bench("geterrmessage", scalar = geterrmessage())
bench("getExportedValue", setup = { package <- "stats"; name <- "median" }, scalar = getExportedValue(package, name))
bench("getHook", setup = { hook <- packageEvent("stats", "onLoad") }, scalar = getHook(hook))
bench("getLoadedDLLs", scalar = getLoadedDLLs())
bench("..getNamespace", setup = { name <- "stats" }, scalar = ..getNamespace(name, "<unknown>"))
bench(".getNamespace", setup = { name <- "stats" }, scalar = .getNamespace(name))
bench("getNamespace", setup = { name <- "stats" }, scalar = getNamespace(name))
bench("getNamespaceExports", setup = { name <- "stats" }, scalar = getNamespaceExports(name))
bench("getNamespaceImports", setup = { name <- "stats" }, scalar = getNamespaceImports(name))
bench(".getNamespaceInfo", setup = { ns <- asNamespace("stats") }, scalar = .getNamespaceInfo(ns, "spec"))
bench("getNamespaceInfo", setup = { name <- "stats" }, scalar = getNamespaceInfo(name, "spec"))
bench("getNamespaceName", setup = { ns <- asNamespace("stats") }, scalar = getNamespaceName(ns))
bench("getNamespaceUsers", setup = { name <- "stats" }, scalar = getNamespaceUsers(name))
bench("getNamespaceVersion", setup = { name <- "stats" }, scalar = getNamespaceVersion(name))
bench("getNativeSymbolInfo", setup = { dll <- getLoadedDLLs()[["stats"]]; name <- "cutree" },
      scalar = getNativeSymbolInfo(name, dll))
bench("getOption", setup = { name <- "digits"; missing_name <- "not.an.option" },
      scalar = getOption(name), default = getOption(missing_name, 7L))
skip(".getRequiredPackages", "reads files")
skip(".getRequiredPackages2", "reads files")
bench("getRversion", scalar = getRversion())
bench("getSrcLines", setup = { source <- srcfilecopy("<text>", txt_1e3); last <- 100L }, small = getSrcLines(source, 1L, last))
bench("getTaskCallbackNames", scalar = getTaskCallbackNames())
bench("gettext", setup = { msg <- "invalid argument" }, scalar = gettext(msg), chr_1e3 = gettext(chr_1e3))
bench("gettextf", setup = { fmt <- "%s has %d values" }, scalar = gettextf(fmt, chr_1, int_1), chr_1e3 = gettextf(fmt, chr_1e3, int_1e3))
bench("getwd", scalar = nzchar(getwd()))
bench("gl", setup = { n <- 10L }, small = gl(n, 2L), len_1e3 = gl(n, 100L), len_1e6 = gl(n, 100000L),
      labels_1e3 = gl(4L, 1L, 1000L, labels = c("low", "mid", "high", "top")))
bench("globalCallingHandlers", scalar = globalCallingHandlers())
bench("globalenv", scalar = globalenv())
bench("gregexec", setup = { pattern <- "([a-z]+)([0-9]*)" }, txt_1e3 = gregexec(pattern, txt_1e3),
      chr_1e3 = gregexec("id([0-9]+)_([a-z])", chr_1e3))
bench("gregexpr", setup = { pattern <- "[aeiou]" }, scalar = gregexpr(pattern, txt_1),
      txt_1e3 = gregexpr(" ", txt_1e3, fixed = TRUE), vowels_1e3 = gregexpr(pattern, txt_1e3),
      perl_1e3 = gregexpr("\\b\\w", txt_1e3, perl = TRUE))
bench("grep", setup = { pattern <- "_[a-c]$" }, scalar = grep(pattern, chr_1),
      chr_1e3 = grep(pattern, chr_1e3), chr_1e6 = grep(pattern, chr_1e6), value_1e3 = grep(pattern, chr_1e3, value = TRUE),
      fixed_1e3 = grep("bond", txt_1e3, fixed = TRUE), perl_1e3 = grep("^id0\\d+", chr_1e3, perl = TRUE),
      fixed_1e6 = grep("_a", chr_1e6, fixed = TRUE), icase_1e3 = grep("BOND", txt_1e3, ignore.case = TRUE))
bench("grepl", setup = { pattern <- "_[a-c]$" }, scalar = grepl(pattern, chr_1),
      chr_1e3 = grepl(pattern, chr_1e3), chr_1e6 = grepl(pattern, chr_1e6),
      fixed_1e3 = grepl("bond", txt_1e3, fixed = TRUE), fixed_1e6 = grepl("_a", chr_1e6, fixed = TRUE),
      perl_1e3 = grepl("^id0\\d+", chr_1e3, perl = TRUE), txt_1e3 = grepl("price|yield", txt_1e3))
bench("grepRaw", setup = { bytes <- charToRaw(paste(txt_1e3, collapse = " ")) },
      first = grepRaw("bond", bytes), all_1e3 = grepRaw("bond", bytes, all = TRUE),
      fixed_1e3 = grepRaw("yield", bytes, fixed = TRUE, all = TRUE))
bench("grepv", setup = { pattern <- "_[a-c]$" }, chr_1e3 = grepv(pattern, chr_1e3), chr_1e6 = grepv(pattern, chr_1e6),
      fixed_1e3 = grepv("bond", txt_1e3, fixed = TRUE))
bench("grouping", int_1e3 = grouping(int_1e3), int_1e6 = grouping(int_1e6), two_1e3 = grouping(fct_1e3, dbl_1e3))
bench("gsub", setup = { pattern <- "[aeiou]" }, scalar = gsub(pattern, "", txt_1),
      txt_1e3 = gsub(pattern, "", txt_1e3), fixed_1e3 = gsub(" ", "_", txt_1e3, fixed = TRUE),
      fixed_1e6 = gsub("_", "-", chr_1e6, fixed = TRUE), perl_1e3 = gsub("(\\w+) (\\w+)", "\\2 \\1", txt_1e3, perl = TRUE),
      backref_1e3 = gsub("id([0-9]+)_([a-z])", "\\2-\\1", chr_1e3), trim_1e3 = gsub("^\\s+|\\s+$", "", txt_1e3))
bench(".gt", setup = { x <- date_1e3 }, date = .gt(x, 1L, 2L))
bench(".gtn", setup = { x <- date_1e3; y <- sort(date_1e3) }, date_1e3 = .gtn(x, TRUE), sorted_1e3 = .gtn(y, FALSE))
skip("gzcon", "reads files")
skip("gzfile", "reads files")
bench(".handleSimpleError", setup = { h <- function(e) conditionMessage(e); msg <- "bad input"; call <- quote(f(x)) },
      scalar = .handleSimpleError(h, msg, call))
bench("I", scalar = I(dbl_1), dbl_1e3 = I(dbl_1e3), df_10 = I(df_10), lst_10 = I(lst_10))
bench("iconv", setup = { from <- "UTF-8"; to <- "latin1" }, scalar = iconv(chr_1, from, to),
      chr_1e3 = iconv(chr_1e3, from, to), txt_1e3 = iconv(txt_1e3, from, "ASCII//TRANSLIT"), chr_1e6 = iconv(chr_1e6, from, to))
skip("iconvlist", "reads files")
bench("icuGetCollate", scalar = icuGetCollate())
skip("icuSetCollate", "changes the session")
bench("identical", setup = {
  dbl_copy <- dbl_1e3 + 0; chr_copy <- paste0(chr_1e3, ""); df_copy <- df_1e3
  df_copy$x <- df_copy$x + 0; f <- function(x) x + 1
}, scalar = identical(dbl_1, dbl_1), dbl_1e3 = identical(dbl_1e3, dbl_copy),
      chr_1e3 = identical(chr_1e3, chr_copy), df_1e3 = identical(df_1e3, df_copy), lst_10 = identical(lst_10, lst_10),
      fun = identical(fun, f), differ = identical(dbl_1e3, int_1e3))
bench("identity", scalar = identity(dbl_1), dbl_1e3 = identity(dbl_1e3))
skip("if", "language construct")
bench("ifelse", scalar = ifelse(lgl_1, dbl_1, 0), lgl_1e3 = ifelse(lgl_1e3, dbl_1e3, 0), lgl_1e6 = ifelse(lgl_1e6, dbl_1e6, 0),
      test_1e3 = ifelse(dbl_1e3 > 0, "pos", "neg"), na_1e3 = ifelse(is.na(na_1e3), 0, na_1e3),
      na_1e6 = ifelse(is.na(na_1e6), 0, na_1e6))
bench("Im", scalar = Im(cpl_1), cpl_1e3 = Im(cpl_1e3), cpl_1e6 = Im(cpl_1e6))
bench("importIntoEnv", setup = { target <- new.env(); ns <- asNamespace("stats"); names <- getNamespaceExports(ns) },
      stats = importIntoEnv(target, names, ns, names))
skip("infoRDS", "reads files")
bench("inherits", setup = { classes <- c("tbl_df", "data.frame") }, df_10 = inherits(df_10, "data.frame"),
      dbl_1 = inherits(dbl_1, "numeric"), which = inherits(df_10, classes, which = TRUE), fct_1e3 = inherits(fct_1e3, classes))
bench("integer", setup = { n <- 1000L; m <- 1000000L }, scalar = integer(int_1), len_1e3 = integer(n), len_1e6 = integer(m))
bench("interaction", setup = { g <- factor(sample(letters[1:5], 1000L, replace = TRUE)) }, fct_1e3 = interaction(fct_1e3, g),
      drop_1e3 = interaction(fct_1e3, g, drop = TRUE), df_1e3 = interaction(df_1e3$g, df_1e3$b))
bench("interactive", scalar = interactive())
bench(".Internal", setup = { n <- 2000L }, dbl_1e3 = .Internal(rep_len(dbl_1e3, n)))
bench("intersect", setup = { other <- rev(int_1e3) + 500L; other_chr <- chr_1e3[seq(1L, 1000L, by = 2L)] },
      int_1e3 = intersect(int_1e3, other), chr_1e3 = intersect(chr_1e3, other_chr), int_1e6 = intersect(int_1e6, int_1e3),
      dbl_1e3 = intersect(dbl_1e3, dbl_1e3[1:500]))
bench("intToBits", scalar = intToBits(int_1), int_1e3 = intToBits(int_1e3), int_1e6 = intToBits(int_1e6))
bench("intToUtf8", int_1e3 = intToUtf8(int_1e3), multiple_1e3 = intToUtf8(int_1e3, multiple = TRUE), scalar = intToUtf8(int_1))
bench("inverse.rle", setup = { r <- rle(sort(int_1e3)) }, int_1e3 = inverse.rle(r))
bench("invisible", scalar = invisible(dbl_1), dbl_1e3 = invisible(dbl_1e3))
bench("invokeRestart", setup = { restart <- function(x) x }, scalar = withRestarts(invokeRestart("use", dbl_1), use = restart))
skip("invokeRestartInteractively", "interactive")
bench("is.array", mat_10 = is.array(mat_10), dbl_1e3 = is.array(dbl_1e3))
bench("is.atomic", dbl_1e3 = is.atomic(dbl_1e3), lst_10 = is.atomic(lst_10))
bench("is.call", setup = { call <- quote(f(x)) }, call = is.call(call), dbl_1 = is.call(dbl_1))
bench("is.character", chr_1e3 = is.character(chr_1e3), dbl_1 = is.character(dbl_1))
bench("is.complex", cpl_1e3 = is.complex(cpl_1e3), dbl_1 = is.complex(dbl_1))
bench("is.data.frame", df_10 = is.data.frame(df_10), lst_10 = is.data.frame(lst_10))
bench("is.double", dbl_1e3 = is.double(dbl_1e3), int_1 = is.double(int_1))
bench("is.element", setup = { table <- int_1e3[1:100] }, scalar = is.element(int_1, table), int_1e3 = is.element(int_1e3, table),
      int_1e6 = is.element(int_1e6, table), chr_1e3 = is.element(chr_1e3, chr_1e3[1:10]))
bench("is.environment", env_1e3 = is.environment(env_1e3), dbl_1 = is.environment(dbl_1))
bench("is.expression", setup = { ex <- expression(a + b) }, expression = is.expression(ex), dbl_1 = is.expression(dbl_1))
bench("is.factor", fct_1e3 = is.factor(fct_1e3), chr_1 = is.factor(chr_1))
bench("is.finite", scalar = is.finite(dbl_1), dbl_1e3 = is.finite(dbl_1e3), dbl_1e6 = is.finite(dbl_1e6),
      na_1e6 = is.finite(na_1e6), int_1e3 = is.finite(int_1e3), cpl_1e3 = is.finite(cpl_1e3))
bench("is.finite.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = is.finite.POSIXlt(lt))
bench("is.function", fun = is.function(fun), dbl_1 = is.function(dbl_1), builtin = is.function(sum))
bench("is.infinite", scalar = is.infinite(dbl_1), dbl_1e3 = is.infinite(dbl_1e3), dbl_1e6 = is.infinite(dbl_1e6))
bench("is.infinite.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = is.infinite.POSIXlt(lt))
bench("is.integer", int_1e3 = is.integer(int_1e3), dbl_1 = is.integer(dbl_1))
bench("is.language", setup = { call <- quote(f(x)) }, call = is.language(call), dbl_1 = is.language(dbl_1))
bench("is.list", lst_10 = is.list(lst_10), df_10 = is.list(df_10), dbl_1 = is.list(dbl_1))
bench("is.loaded", setup = { name <- "cutree" }, scalar = is.loaded(name), package = is.loaded(name, PACKAGE = "stats"))
bench("is.logical", lgl_1e3 = is.logical(lgl_1e3), dbl_1 = is.logical(dbl_1))
bench("is.matrix", mat_10 = is.matrix(mat_10), df_10 = is.matrix(df_10))
bench("is.na", scalar = is.na(dbl_1), na_1e3 = is.na(na_1e3), na_1e6 = is.na(na_1e6), int_1e6 = is.na(int_1e6),
      chr_1e3 = is.na(chr_1e3), chr_1e6 = is.na(chr_1e6), lst_10 = is.na(lst_10), fct_1e3 = is.na(fct_1e3))
bench("is.na.data.frame", df_10 = is.na.data.frame(df_10), df_1e3 = is.na.data.frame(df_1e3))
bench("is.na.numeric_version", setup = { v <- numeric_version(sprintf("%d.%d", int_1e3 %% 10L, int_1e3 %% 7L)) },
      ver_1e3 = is.na.numeric_version(v))
bench("is.na.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = is.na.POSIXlt(lt))
bench("is.na<-", setup = { y <- dbl_1e3; idx <- seq(1L, 1000L, by = 10L) }, dbl_1e3 = is.na(y) <- idx)
bench("is.na<-.default", setup = { y <- dbl_1e3; idx <- seq(1L, 1000L, by = 10L) }, dbl_1e3 = `is.na<-.default`(y, idx))
bench("is.na<-.factor", setup = { y <- fct_1e3; idx <- seq(1L, 1000L, by = 10L) }, fct_1e3 = `is.na<-.factor`(y, idx))
bench("is.na<-.numeric_version", setup = { v <- numeric_version(sprintf("%d.%d", int_1e3 %% 10L, int_1e3 %% 7L)); idx <- 1:10 },
      ver_1e3 = `is.na<-.numeric_version`(v, idx))
bench("is.name", setup = { name <- quote(x) }, symbol = is.name(name), dbl_1 = is.name(dbl_1))
bench("is.nan", scalar = is.nan(dbl_1), dbl_1e3 = is.nan(dbl_1e3), dbl_1e6 = is.nan(dbl_1e6))
bench("is.nan.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = is.nan.POSIXlt(lt))
bench("is.null", setup = { nothing <- NULL }, null = is.null(nothing), dbl_1 = is.null(dbl_1), element = is.null(lst_10$e))
bench("is.numeric", dbl_1e3 = is.numeric(dbl_1e3), int_1 = is.numeric(int_1), fct_1e3 = is.numeric(fct_1e3), chr_1 = is.numeric(chr_1))
bench("is.numeric.Date", date_1e3 = is.numeric.Date(date_1e3))
bench("is.numeric.difftime", setup = { span <- as.difftime(dbl_1e3, units = "secs") }, dbl_1e3 = is.numeric.difftime(span))
bench("is.numeric.POSIXt", time_1e3 = is.numeric.POSIXt(time_1e3))
bench("is.numeric_version", setup = { v <- numeric_version("1.2.3") }, version = is.numeric_version(v), chr_1 = is.numeric_version(chr_1))
bench("is.object", df_10 = is.object(df_10), dbl_1 = is.object(dbl_1))
bench("is.ordered", setup = { o <- factor(letters, ordered = TRUE) }, ordered = is.ordered(o), fct_1e3 = is.ordered(fct_1e3))
bench("is.package_version", setup = { v <- package_version("1.2.3") }, version = is.package_version(v), chr_1 = is.package_version(chr_1))
bench("is.pairlist", setup = { p <- formals(fun) }, pairlist = is.pairlist(p), lst_10 = is.pairlist(lst_10))
bench("is.primitive", builtin = is.primitive(sum), fun = is.primitive(fun))
bench("is.qr", setup = { q <- qr(mat_10) }, qr = is.qr(q), mat_10 = is.qr(mat_10))
skip("is.R", "defunct")
bench("is.raw", raw_1e3 = is.raw(raw_1e3), dbl_1 = is.raw(dbl_1))
bench("is.recursive", lst_10 = is.recursive(lst_10), dbl_1e3 = is.recursive(dbl_1e3))
skip("is.single", "defunct")
bench("is.symbol", setup = { name <- quote(x) }, symbol = is.symbol(name), dbl_1 = is.symbol(dbl_1))
bench("is.table", setup = { t <- table(fct_1e3) }, table = is.table(t), dbl_1 = is.table(dbl_1))
bench("is.unsorted", sorted_1e3 = is.unsorted(sorted_1e3), sorted_1e6 = is.unsorted(sorted_1e6), dbl_1e6 = is.unsorted(dbl_1e6),
      chr_1e3 = is.unsorted(chr_1e3), strictly_1e3 = is.unsorted(sorted_1e3, strictly = TRUE))
bench("is.vector", dbl_1e3 = is.vector(dbl_1e3), mode = is.vector(lst_10, mode = "list"), fct_1e3 = is.vector(fct_1e3))
bench("isa", setup = { classes <- c("tbl_df", "data.frame") }, df_10 = isa(df_10, "data.frame"), several = isa(df_10, classes))
bench("isatty", setup = { con <- stdout() }, stdout = isatty(con))
bench("isBaseNamespace", setup = { ns <- asNamespace("stats") }, scalar = isBaseNamespace(ns))
bench("isdebugged", scalar = isdebugged(fun))
bench("isFALSE", scalar = isFALSE(lgl_1), dbl_1 = isFALSE(dbl_1))
bench("isIncomplete", setup = { con <- stdout() }, stdout = isIncomplete(con))
bench(".isMethodsDispatchOn", scalar = .isMethodsDispatchOn())
bench("isNamespace", setup = { ns <- asNamespace("stats") }, namespace = isNamespace(ns), env_1e3 = isNamespace(env_1e3))
bench("isNamespaceLoaded", setup = { name <- "stats"; missing_name <- "notapackage" },
      loaded = isNamespaceLoaded(name), missing = isNamespaceLoaded(missing_name))
bench("ISOdate", setup = { years <- 1950L + int_1e3 %% 70L; months <- 1L + int_1e3 %% 12L; days <- 1L + int_1e3 %% 28L },
      scalar = ISOdate(2021L, 6L, int_1), int_1e3 = ISOdate(years, months, days))
bench("ISOdatetime", setup = { years <- 1950L + int_1e3 %% 70L; months <- 1L + int_1e3 %% 12L; days <- 1L + int_1e3 %% 28L },
      scalar = ISOdatetime(2021L, 6L, int_1, 12L, 30L, 0, tz = "UTC"),
      int_1e3 = ISOdatetime(years, months, days, int_1e3 %% 24L, int_1e3 %% 60L, 0, tz = "UTC"))
bench(".isOpen", setup = { source <- srcfilecopy("<text>", txt_1e3[1:10]) }, scalar = .isOpen(source))
bench("isOpen", setup = { con <- stdout() }, stdout = isOpen(con))
bench("isRestart", setup = { restart <- computeRestarts()[[1L]] }, restart = isRestart(restart), dbl_1 = isRestart(dbl_1))
