skip("return", "language construct")
bench("returnValue", scalar = returnValue(chr_1))
bench("rev", dbl_1e3 = rev(dbl_1e3), dbl_1e6 = rev(dbl_1e6), chr_1e3 = rev(chr_1e3), lst_1e3 = rev(lst_1e3))
bench("rev.default", dbl_1e3 = rev.default(dbl_1e3))
bench("rle", setup = { s <- sort(int_1e3) }, int_1e3 = rle(s), lgl_1e3 = rle(lgl_1e3), lgl_1e6 = rle(lgl_1e6))
bench("rm", setup = { e <- new.env() }, scalar = { assign("a", dbl_1, envir = e); rm("a", envir = e) })
bench(".rmpkg", setup = { p <- "package:stats" }, scalar = .rmpkg(p))
bench("RNGkind", setup = { k <- NULL }, scalar = RNGkind(kind = k))
bench("RNGversion", setup = { v <- "4.0.0" }, scalar = RNGversion(v))
bench("round", dbl_1 = round(dbl_1, 2), dbl_1e3 = round(dbl_1e3), digits_1e3 = round(dbl_1e3, 2),
      dbl_1e6 = round(dbl_1e6, 2))
bench("round.Date", date_1e3 = round(date_1e3))
bench("round.POSIXt", time_1 = round.POSIXt(time_1, "mins"), time_1e3 = round.POSIXt(time_1e3, "hours"))
bench(".row", setup = { d <- dim(mat_100) }, mat_100 = .row(d))
bench("row", mat_100 = row(mat_100), mat_1000_1e6 = row(mat_1000))
bench("row.names", df_10 = row.names(df_10), df_1e3 = row.names(df_1e3))
bench("row.names.data.frame", df_1e3 = row.names.data.frame(df_1e3))
bench("row.names.default", mat_100 = row.names.default(mat_100))
bench("row.names<-", setup = { y <- df_1e3; rn <- paste0("r", seq_len(1000L)) }, df_1e3 = row.names(y) <- rn)
bench("row.names<-.data.frame", setup = { y <- df_1e3; rn <- paste0("r", seq_len(1000L)) },
      df_1e3 = y <- `row.names<-.data.frame`(y, value = rn))
bench("row.names<-.default", setup = { m <- mat_100; rn <- paste0("r", seq_len(100L)) },
      mat_100 = m <- `row.names<-.default`(m, value = rn))
bench(".row_names_info", df_1e3 = .row_names_info(df_1e3), type_1e3 = .row_names_info(df_1e3, 2L))
bench(".rowMeans", mat_100 = .rowMeans(mat_100, 100L, 100L), mat_1000_1e6 = .rowMeans(mat_1000, 1000L, 1000L))
bench("rowMeans", setup = { xy <- df_1e3[c("x", "y")] }, mat_10 = rowMeans(mat_10), mat_100 = rowMeans(mat_100),
      mat_1000_1e6 = rowMeans(mat_1000), df_1e3 = rowMeans(xy))
bench("rownames", mat_100 = rownames(mat_100), df_1e3 = rownames(df_1e3))
bench("rownames<-", setup = { m <- mat_100; rn <- paste0("r", seq_len(100L)) }, mat_100 = rownames(m) <- rn)
bench(".rowNamesDF<-", setup = { y <- df_1e3; rn <- paste0("r", seq_len(1000L)) },
      df_1e3 = y <- `.rowNamesDF<-`(y, value = rn))
bench("rowsum", setup = { groups <- rep(1:10, 10L) }, dbl_1e3 = rowsum(dbl_1e3, fct_1e3),
      mat_100 = rowsum(mat_100, groups), dbl_1e6 = rowsum(dbl_1e6, int_1e6))
bench("rowsum.data.frame", setup = { xy <- df_1e3[c("x", "y")] }, df_1e3 = rowsum.data.frame(xy, df_1e3$g))
bench("rowsum.default", dbl_1e3 = rowsum.default(dbl_1e3, fct_1e3))
bench(".rowSums", mat_100 = .rowSums(mat_100, 100L, 100L), mat_1000_1e6 = .rowSums(mat_1000, 1000L, 1000L))
bench("rowSums", setup = { xy <- df_1e3[c("x", "y")] }, mat_10 = rowSums(mat_10), mat_100 = rowSums(mat_100),
      imat_100 = rowSums(imat_100), mat_1000_1e6 = rowSums(mat_1000), df_1e3 = rowSums(xy))
bench(".S3method", setup = { cls <- "rybench_s3class"; method <- function(x, ...) x },
      scalar = .S3method("print", cls, method))
bench("sample", setup = { n <- 10L }, scalar = sample(n), int_1e3 = sample(int_1e3),
      chr_1e3 = sample(chr_1e3, 10L), int_1e3_replace = sample(int_1e3, 1000L, replace = TRUE),
      int_1e3_prob = sample(int_1e3, 100L, replace = TRUE, prob = pos_1e3), int_1e6 = sample(int_1e6))
bench("sample.int", setup = { n <- 1000L; n6 <- 1000000L }, int_1e3 = sample.int(n), size_1e3 = sample.int(n, 10L),
      int_1e6 = sample.int(n6))
bench("sapply", setup = { pair <- function(x) c(x, x) }, lst_1e3 = sapply(lst_1e3, fun),
      chr_1e3 = sapply(chr_1e3, nchar), matrix_1e3 = sapply(lst_1e3, pair))
bench("save", setup = { y <- dbl_1e3 },
      dbl_1e3 = { con <- rawConnection(raw(0L), "wb"); save(y, file = con); close(con) })
skip("save.image", "writes files")
bench("saveRDS", dbl_1e3 = { con <- rawConnection(raw(0L), "wb"); saveRDS(dbl_1e3, con); close(con) },
      df_1e3 = { con <- rawConnection(raw(0L), "wb"); saveRDS(df_1e3, con); close(con) })
bench("scale", mat_100 = scale(mat_100), dbl_1e3 = scale(dbl_1e3), mat_1000_1e6 = scale(mat_1000))
bench("scale.default", mat_100 = scale.default(mat_100))
bench("scan", numstr_1e3 = scan(text = numstr_1e3, quiet = TRUE),
      txt_1e3 = scan(text = txt_1e3, what = "", quiet = TRUE))
skip(".Script", "runs a process")
bench("search", scalar = search())
bench("searchpaths", scalar = searchpaths())
bench("seek", raw_1e3 = { con <- rawConnection(raw_1e3); seek(con, 10L); position <- seek(con); close(con); position })
bench("seek.connection", raw_1e3 = { con <- rawConnection(raw_1e3); position <- seek.connection(con, 10L); close(con); position })
bench("seq", setup = { n <- 1000; n6 <- 1e6 }, scalar = seq(int_1), to_1e3 = seq(1, n), by_1e3 = seq(0, n, by = 1),
      length_out_1e3 = seq(0, 1, length.out = n), along_1e3 = seq(along.with = dbl_1e3),
      to_1e6 = seq(1, n6), by_1e6 = seq(0, n6, by = 2))
bench("seq.Date", setup = { n <- 1000L }, day_1e3 = seq(date_1, by = "day", length.out = n),
      month_1e3 = seq(date_1, by = "month", length.out = n))
bench("seq.default", setup = { n <- 1000 }, to_1e3 = seq.default(1, n), by_1e3 = seq.default(0, 1, by = 0.001))
bench("seq.int", setup = { n <- 1000L; n6 <- 1000000L }, scalar = seq.int(1L, int_1), to_1e3 = seq.int(1L, n),
      length_out_1e3 = seq.int(0, 1, length.out = n), to_1e6 = seq.int(1L, n6))
bench("seq.POSIXt", setup = { n <- 1000L }, hour_1e3 = seq(time_1, by = "hour", length.out = n),
      day_1e3 = seq.POSIXt(time_1, by = "day", length.out = n))
bench("seq_along", dbl_1 = seq_along(dbl_1), dbl_1e3 = seq_along(dbl_1e3), lst_1e3 = seq_along(lst_1e3),
      dbl_1e6 = seq_along(dbl_1e6))
bench("seq_len", setup = { n <- 1000L; n6 <- 1000000L }, scalar = seq_len(int_1), len_1e3 = seq_len(n),
      len_1e6 = seq_len(n6))
bench("sequence", setup = { small <- 1:10 }, small = sequence(small), int_1e3 = sequence(int_1e3))
bench("sequence.default", int_1e3 = sequence.default(int_1e3))
bench("serialize", dbl_1e3 = serialize(dbl_1e3, NULL), chr_1e3 = serialize(chr_1e3, NULL),
      lst_1e3 = serialize(lst_1e3, NULL), df_1e3 = serialize(df_1e3, NULL), dbl_1e6 = serialize(dbl_1e6, NULL))
skip("serverSocket", "network")
bench("set.seed", scalar = set.seed(int_1), kind = set.seed(int_1, kind = "Mersenne-Twister"))
bench(".set_ops_need_as_vector", int_1e3 = .set_ops_need_as_vector(int_1e3, int_1e3))
bench(".set_row_names", setup = { n <- 1000L }, scalar = .set_row_names(n))
bench("setdiff", setup = { half <- int_1e3[1:500]; chr_half <- chr_1e3[1:500]; dbl_half <- dbl_1e6[1:500000] },
      int_1e3 = setdiff(int_1e3, half), chr_1e3 = setdiff(chr_1e3, chr_half), dbl_1e6 = setdiff(dbl_1e6, dbl_half))
bench("setequal", setup = { y <- rev(int_1e3); chr_y <- rev(chr_1e3) }, int_1e3 = setequal(int_1e3, y),
      chr_1e3 = setequal(chr_1e3, chr_y))
bench("setHook", setup = { hook <- "rybench.hook" }, scalar = setHook(hook, fun, "replace"))
bench("setNamespaceInfo", setup = {
  ns <- new.env()
  info <- new.env()
  info$spec <- c(name = "rybench", version = "1.0")
  ns$.__NAMESPACE__. <- info
}, scalar = setNamespaceInfo(ns, "rybench", dbl_1))
bench("setSessionTimeLimit", setup = { limit <- Inf }, scalar = setSessionTimeLimit(cpu = limit, elapsed = limit))
bench("setTimeLimit", setup = { limit <- Inf }, scalar = setTimeLimit(cpu = limit, elapsed = limit))
skip("setwd", "changes the session")
bench("showConnections", setup = { all <- TRUE }, scalar = showConnections(all = all))
bench("shQuote", chr_1 = shQuote(chr_1), chr_1e3 = shQuote(chr_1e3))
bench("sign", dbl_1 = sign(dbl_1), dbl_1e3 = sign(dbl_1e3), int_1e3 = sign(int_1e3), dbl_1e6 = sign(dbl_1e6))
bench("signalCondition", setup = { condition <- simpleCondition("rybench") }, scalar = signalCondition(condition))
bench(".signalSimpleWarning", setup = { text <- "rybench"; where <- quote(f()) },
      scalar = suppressWarnings(.signalSimpleWarning(text, where)))
bench("signif", dbl_1 = signif(dbl_1, 3), dbl_1e3 = signif(dbl_1e3, 3), dbl_1e6 = signif(dbl_1e6, 3))
bench("simpleCondition", scalar = simpleCondition(chr_1))
bench("simpleError", scalar = simpleError(chr_1))
bench("simpleMessage", scalar = simpleMessage(chr_1))
bench("simpleWarning", scalar = simpleWarning(chr_1))
bench("simplify2array", setup = { vectors <- rep(list(1:3), 1000L) }, lst_1e3 = simplify2array(lst_1e3),
      matrix_1e3 = simplify2array(vectors))
bench("sin", dbl_1 = sin(dbl_1), dbl_1e3 = sin(dbl_1e3), dbl_1e6 = sin(dbl_1e6))
bench("single", setup = { n <- 1000L }, len_1e3 = single(n))
bench("sinh", setup = { x <- dbl_1e3 / 10; x6 <- dbl_1e6 / 10 }, dbl_1 = sinh(dbl_1), dbl_1e3 = sinh(x), dbl_1e6 = sinh(x6))
bench("sink", scalar = { con <- textConnection(NULL, "w"); sink(con); sink(); close(con) })
bench("sink.number", setup = { type <- "output" }, scalar = sink.number(type))
bench("sinpi", dbl_1 = sinpi(dbl_1), dbl_1e3 = sinpi(dbl_1e3), dbl_1e6 = sinpi(dbl_1e6))
bench("slice.index", mat_100 = slice.index(mat_100, 2L))
skip("socketAccept", "network")
skip("socketConnection", "network")
skip("socketSelect", "network")
skip("socketTimeout", "network")
bench("solve", setup = { b <- dbl_1e3[1:100]; m10 <- mat_10 + diag(10L) }, mat_10 = solve(m10),
      spd_100 = solve(spd_100), system_100 = solve(spd_100, b))
bench("solve.default", spd_100 = solve.default(spd_100))
bench("solve.qr", setup = { decomposition <- qr(spd_100); b <- dbl_1e3[1:100] }, spd_100 = solve.qr(decomposition, b))
bench("sort", dbl_1e3 = sort(dbl_1e3), int_1e3 = sort(int_1e3), chr_1e3 = sort(chr_1e3), fct_1e3 = sort(fct_1e3),
      decreasing_1e3 = sort(dbl_1e3, decreasing = TRUE), dbl_1e6 = sort(dbl_1e6), int_1e6 = sort(int_1e6))
bench("sort.default", dbl_1e3 = sort.default(dbl_1e3))
bench("sort.int", dbl_1e3 = sort.int(dbl_1e3), int_1e3 = sort.int(int_1e3))
bench("sort.list", dbl_1e3 = sort.list(dbl_1e3), chr_1e3 = sort.list(chr_1e3))
bench("sort.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = sort.POSIXlt(lt))
bench("sort_by", df_1e3 = sort_by(df_1e3, ~ x), dbl_1e3 = sort_by(dbl_1e3, int_1e3))
bench("sort_by.data.frame", df_1e3 = sort_by.data.frame(df_1e3, ~ g + x))
bench("sort_by.default", dbl_1e3 = sort_by.default(dbl_1e3, int_1e3))
bench("source", setup = { code <- parse(text = c("a <- 1", "b <- a + 1")); local <- new.env() },
      small = source(exprs = code, local = local))
bench("split", dbl_1e3 = split(dbl_1e3, fct_1e3), chr_1e3 = split(chr_1e3, int_1e3),
      df_1e3 = split(df_1e3, df_1e3$g), dbl_1e6 = split(dbl_1e6, fct_1e6))
bench("split.data.frame", df_1e3 = split.data.frame(df_1e3, df_1e3$g))
bench("split.Date", date_1e3 = split.Date(date_1e3, fct_1e3))
bench("split.default", dbl_1e3 = split.default(dbl_1e3, fct_1e3))
bench("split.POSIXct", time_1e3 = split.POSIXct(time_1e3, fct_1e3))
bench("split<-", setup = { y <- dbl_1e3; parts <- split(dbl_1e3, fct_1e3) }, dbl_1e3 = split(y, fct_1e3) <- parts)
bench("split<-.data.frame", setup = { y <- df_1e3; parts <- split(df_1e3, df_1e3$g) },
      df_1e3 = y <- `split<-.data.frame`(y, df_1e3$g, value = parts))
bench("split<-.default", setup = { y <- dbl_1e3; parts <- split(dbl_1e3, fct_1e3) },
      dbl_1e3 = y <- `split<-.default`(y, fct_1e3, value = parts))
bench("sprintf", scalar = sprintf("%d items of %s", int_1, chr_1), int_1e3 = sprintf("%05d", int_1e3),
      dbl_1e3 = sprintf("%.2f", dbl_1e3), chr_1e3 = sprintf("%s_x", chr_1e3),
      multi_1e3 = sprintf("%s: %d (%.1f)", chr_1e3, int_1e3, dbl_1e3))
bench("sqrt", dbl_1 = sqrt(dbl_1), pos_1e3 = sqrt(pos_1e3), int_1e3 = sqrt(int_1e3), pos_1e6 = sqrt(pos_1e6))
bench("sQuote", chr_1 = sQuote(chr_1, FALSE), chr_1e3 = sQuote(chr_1e3, FALSE))
bench("srcfile", setup = { name <- "rybench.R" }, scalar = srcfile(name))
bench("srcfilealias", setup = { name <- "alias.R"; file <- srcfile("rybench.R") }, scalar = srcfilealias(name, file))
bench("srcfilecopy", setup = { name <- "rybench.R"; stamp <- as.POSIXct("2021-01-01", tz = "UTC") },
      txt_1e3 = srcfilecopy(name, txt_1e3, timestamp = stamp))
bench("srcref", setup = { file <- srcfilecopy("rybench.R", c("a <- 1", "b <- 2")); range <- c(1L, 1L, 1L, 6L) },
      scalar = srcref(file, range))
bench(".standard_regexps", scalar = .standard_regexps())
bench("standardGeneric", setup = {
  setGeneric("rybenchGeneric", function(x) standardGeneric("rybenchGeneric"), where = environment())
  setMethod("rybenchGeneric", "numeric", function(x) x + 1, where = environment())
}, dbl_1 = rybenchGeneric(dbl_1), dbl_1e3 = rybenchGeneric(dbl_1e3))
bench("startsWith", chr_1 = startsWith(chr_1, "id0"), chr_1e3 = startsWith(chr_1e3, "id0"),
      chr_1e6 = startsWith(chr_1e6, "id0"))
bench("stderr", scalar = stderr())
bench("stdin", scalar = stdin())
bench("stdout", scalar = stdout())
bench("stop", setup = { condition <- simpleError("rybench") },
      chr_1 = tryCatch(stop(chr_1), error = function(e) NULL),
      condition = tryCatch(stop(condition), error = function(e) NULL))
bench("stopifnot", lgl_1 = stopifnot(lgl_1), checks = stopifnot(is.numeric(dbl_1e3), length(dbl_1e3) > 0L),
      dbl_1e3 = stopifnot(dbl_1e3 > -101))
bench("storage.mode", dbl_1e3 = storage.mode(dbl_1e3), lst_10 = storage.mode(lst_10))
bench("storage.mode<-", int_1e3 = { y <- int_1e3; storage.mode(y) <- "double" },
      int_1e6 = { y <- int_1e6; storage.mode(y) <- "double" })
bench("str2expression", setup = { text <- c("x + y * 2", "f(a, b = 1)") }, small = str2expression(text))
bench("str2lang", setup = { text <- "x + y * 2" }, scalar = str2lang(text))
bench("strftime", time_1 = strftime(time_1, "%Y-%m-%d %H:%M", tz = "UTC"),
      time_1e3 = strftime(time_1e3, "%Y-%m-%d", tz = "UTC"))
bench("strptime", datestr_1 = strptime(datestr_1e3[1L], "%Y-%m-%d", tz = "UTC"),
      datestr_1e3 = strptime(datestr_1e3, "%Y-%m-%d", tz = "UTC"))
bench("strrep", setup = { counts <- int_1e3 %% 20L }, chr_1e3 = strrep(chr_1e3, 3L), int_1e3 = strrep("ab", counts))
bench("strsplit", txt_1 = strsplit(txt_1, " "), txt_1e3 = strsplit(txt_1e3, " "),
      fixed_1e3 = strsplit(chr_1e3, "_", fixed = TRUE), regex_1e3 = strsplit(txt_1e3, "[aeiou]+"))
bench("strtoi", setup = { decimal <- as.character(int_1e3); hex <- sprintf("%x", int_1e3) },
      decimal_1e3 = strtoi(decimal, 10L), hex_1e3 = strtoi(hex, 16L))
bench("strtrim", chr_1e3 = strtrim(chr_1e3, 4L))
bench("structure", setup = { dims <- c(10L, 100L) }, scalar = structure(dbl_1, class = "rybench"),
      dbl_1e3 = structure(dbl_1e3, class = "rybench"), lst_10 = structure(lst_10, class = c("a", "b")),
      dim_1e3 = structure(int_1e3, dim = dims), dbl_1e6 = structure(dbl_1e6, unit = "m"))
bench("strwrap", setup = { paragraph <- paste(txt_1e3[1:100], collapse = " ") }, small = strwrap(paragraph, 40),
      txt_1e3 = strwrap(txt_1e3, 20))
bench("sub", txt_1 = sub("the", "a", txt_1), txt_1e3 = sub("the", "a", txt_1e3),
      regex_1e3 = sub("^([a-z]+) ", "\\1_", txt_1e3), fixed_1e3 = sub("_", "-", chr_1e3, fixed = TRUE),
      fixed_1e6 = sub("_", "-", chr_1e6, fixed = TRUE))
bench(".subset", df_1e3 = .subset(df_1e3, "x"), dbl_1e3 = .subset(dbl_1e3, 1:10))
bench("subset", df_1e3 = subset(df_1e3, x > 0), select_1e3 = subset(df_1e3, x > 0, select = c(id, g)),
      dbl_1e3 = subset(dbl_1e3, dbl_1e3 > 0))
bench("subset.data.frame", df_1e3 = subset.data.frame(df_1e3, x > 0))
bench("subset.default", dbl_1e3 = subset.default(dbl_1e3, dbl_1e3 > 0))
bench("subset.matrix", mat_100 = subset.matrix(mat_100, mat_100[, 1L] > 0))
bench(".subset2", df_1e3 = .subset2(df_1e3, "x"), lst_1e3 = .subset2(lst_1e3, 5L))
bench("substitute", setup = { env <- list(a = 1, b = quote(z)); f <- function(x) substitute(x) },
      env = substitute(a + b, env), promise = f(dbl_1 + 1))
bench("substr", chr_1 = substr(chr_1, 2L, 5L), chr_1e3 = substr(chr_1e3, 2L, 5L), chr_1e6 = substr(chr_1e6, 2L, 5L))
bench("substr<-", chr_1e3 = { y <- chr_1e3; substr(y, 1L, 2L) <- "ZZ" })
bench("substring", setup = { first <- 1:5; last <- 3:7 }, chr_1 = substring(chr_1, first, last),
      chr_1e3 = substring(chr_1e3, 3L))
bench("substring<-", chr_1e3 = { y <- chr_1e3; substring(y, 1L) <- "ZZ" })
bench("sum", dbl_1 = sum(dbl_1), dbl_1e3 = sum(dbl_1e3), int_1e3 = sum(int_1e3), na_1e3 = sum(na_1e3, na.rm = TRUE),
      args_1e3 = sum(dbl_1e3, int_1e3, dbl_1), dbl_1e6 = sum(dbl_1e6), int_1e6 = sum(int_1e6), lgl_1e6 = sum(lgl_1e6))
bench("summary", dbl_1e3 = summary(dbl_1e3), fct_1e3 = summary(fct_1e3), lgl_1e3 = summary(lgl_1e3),
      chr_1e3 = summary(chr_1e3), df_1e3 = summary(df_1e3), dbl_1e6 = summary(dbl_1e6))
bench("summary.connection", setup = { con <- stdout() }, scalar = summary.connection(con))
bench("Summary.data.frame", setup = { xy <- df_1e3[c("x", "y")] }, df_1e3 = max(xy), range_1e3 = range(xy))
bench("summary.data.frame", df_10 = summary.data.frame(df_10), df_1e3 = summary.data.frame(df_1e3))
bench("Summary.Date", date_1e3 = max(date_1e3), range_1e3 = range(date_1e3))
bench("summary.Date", date_1e3 = summary.Date(date_1e3))
bench("summary.default", dbl_1e3 = summary.default(dbl_1e3), dbl_1e6 = summary.default(dbl_1e6))
bench("Summary.difftime", setup = { dt <- difftime(time_1e3, time_1, units = "days") }, time_1e3 = sum(dt))
bench("summary.difftime", setup = { dt <- difftime(time_1e3, time_1, units = "days") }, time_1e3 = summary.difftime(dt))
skip("Summary.factor", "only stops with an error")
bench("summary.factor", fct_1e3 = summary.factor(fct_1e3), fct_1e6 = summary.factor(fct_1e6))
bench("summary.matrix", mat_100 = summary.matrix(mat_100))
bench("Summary.numeric_version", setup = { versions <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 5L, int_1e3 %% 13L, int_1e3)) },
      ver_1e3 = max(versions))
bench("Summary.ordered", setup = { ordered <- as.ordered(fct_1e3) }, fct_1e3 = max(ordered))
bench("Summary.POSIXct", time_1e3 = max(time_1e3), range_1e3 = range(time_1e3))
bench("summary.POSIXct", time_1e3 = summary.POSIXct(time_1e3))
bench("Summary.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = max(lt))
bench("summary.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = summary.POSIXlt(lt))
bench("summary.proc_time", setup = {
  times <- structure(c(1, 2, 3, 0, 0), names = c("user.self", "sys.self", "elapsed", "user.child", "sys.child"),
                     class = "proc_time")
}, scalar = summary.proc_time(times))
bench("summary.srcfile", setup = { file <- srcfilecopy("rybench.R", c("a <- 1", "b <- 2"),
                                                       timestamp = as.POSIXct("2021-01-01", tz = "UTC")) },
      scalar = summary.srcfile(file))
bench("summary.srcref", setup = { ref <- srcref(srcfilecopy("rybench.R", c("a <- 1", "b <- 2")), c(1L, 1L, 1L, 6L)) },
      scalar = summary.srcref(ref))
bench("summary.table", setup = { tab <- table(fct_1e3, lgl_1e3) }, fct_1e3 = summary.table(tab))
bench("summary.warnings", setup = {
  warned <- structure(list(quote(f(x)), NULL, quote(f(x))), names = c("w1", "w2", "w1"), class = "warnings")
}, small = summary.warnings(warned))
bench("suppressMessages", scalar = suppressMessages(message(chr_1)), quiet = suppressMessages(fun(dbl_1)))
bench("suppressPackageStartupMessages", scalar = suppressPackageStartupMessages(packageStartupMessage(chr_1)))
bench("suppressWarnings", scalar = suppressWarnings(fun(dbl_1)), warns_1e3 = suppressWarnings(as.numeric(chr_1e3)))
bench("suspendInterrupts", scalar = suspendInterrupts(fun(dbl_1)))
bench("svd", mat_10 = svd(mat_10), mat_100 = svd(mat_100))
bench("sweep", setup = { centers <- colMeans(mat_100); centers_1000 <- colMeans(mat_1000) },
      mat_100 = sweep(mat_100, 2L, centers), mat_1000_1e6 = sweep(mat_1000, 2L, centers_1000))
bench("switch", setup = { key <- "c"; index <- 2L }, chr_1 = switch(key, a = 1, b = 2, c = 3, 4),
      int_1 = switch(index, "a", "b", "c"), fallthrough = switch(key, a = , b = , c = "abc", "other"))
bench("sys.call", setup = { f <- function(x) sys.call() }, scalar = f(dbl_1))
bench("sys.calls", setup = { f <- function(x) sys.calls() }, scalar = f(dbl_1))
skip("Sys.chmod", "writes files")
skip("Sys.Date", "nondeterministic")
bench("sys.frame", setup = { which <- 0L; f <- function(x) sys.frame(-1L) }, global = sys.frame(which), parent = f(dbl_1))
bench("sys.frames", setup = { f <- function(x) sys.frames() }, scalar = f(dbl_1))
bench("sys.function", setup = { f <- function(x) sys.function() }, scalar = f(dbl_1))
bench("Sys.getenv", setup = { name <- "HOME"; names <- c("HOME", "PATH", "RYBENCH_UNSET") },
      scalar = Sys.getenv(name), small = Sys.getenv(names))
bench("Sys.getlocale", setup = { category <- "LC_COLLATE" }, scalar = Sys.getlocale(category))
skip("Sys.getpid", "nondeterministic")
skip("Sys.glob", "reads files")
bench("Sys.info", scalar = Sys.info())
skip("sys.load.image", "reads files")
bench("Sys.localeconv", scalar = Sys.localeconv())
bench("sys.nframe", setup = { f <- function(x) sys.nframe() }, scalar = f(dbl_1))
bench("sys.on.exit", setup = { f <- function(x) { on.exit(x); sys.on.exit() } }, scalar = f(dbl_1))
bench("sys.parent", setup = { f <- function(x) sys.parent() }, scalar = f(dbl_1))
bench("sys.parents", setup = { f <- function(x) sys.parents() }, scalar = f(dbl_1))
skip("Sys.readlink", "reads files")
skip("sys.save.image", "writes files")
skip("Sys.setenv", "changes the session")
skip("Sys.setFileTime", "writes files")
skip("Sys.setLanguage", "changes the session")
skip("Sys.setlocale", "changes the session")
skip("Sys.sleep", "sleeps")
skip("sys.source", "reads files")
bench("sys.status", setup = { f <- function(x) sys.status() }, scalar = f(dbl_1))
skip("Sys.time", "nondeterministic")
skip("Sys.timezone", "reads files")
bench("Sys.umask", setup = { mode <- NA }, scalar = Sys.umask(mode))
bench("Sys.unsetenv", setup = { name <- "RYBENCH_UNSET" }, scalar = Sys.unsetenv(name))
skip("Sys.which", "reads files")
skip("system", "runs a process")
skip("system.file", "reads files")
skip("system.time", "nondeterministic")
skip("system2", "runs a process")
bench("t", dbl_1e3 = t(dbl_1e3), mat_100 = t(mat_100), df_10 = t(df_10), mat_1000_1e6 = t(mat_1000))
bench("t.data.frame", df_10 = t.data.frame(df_10), df_1e3 = t.data.frame(df_1e3))
bench("t.default", mat_100 = t.default(mat_100))
bench("table", fct_1e3 = table(fct_1e3), int_1e3 = table(int_1e3), chr_1e3 = table(chr_1e3),
      two_way_1e3 = table(fct_1e3, lgl_1e3), fct_1e6 = table(fct_1e6))
bench("tabulate", int_1e3 = tabulate(int_1e3), nbins_1e3 = tabulate(int_1e3, 1000L), int_1e6 = tabulate(int_1e6))
bench("Tailcall", setup = { n <- 100L; f <- function(n) if (n > 0L) Tailcall(f, n - 1L) else 0L }, small = f(n))
bench("tan", dbl_1 = tan(dbl_1), dbl_1e3 = tan(dbl_1e3), dbl_1e6 = tan(dbl_1e6))
bench("tanh", dbl_1 = tanh(dbl_1), dbl_1e3 = tanh(dbl_1e3), dbl_1e6 = tanh(dbl_1e6))
bench("tanpi", dbl_1 = tanpi(dbl_1), dbl_1e3 = tanpi(dbl_1e3), dbl_1e6 = tanpi(dbl_1e6))
bench(".TAOCP1997init", setup = { seed <- 42L }, scalar = .TAOCP1997init(seed))
bench("tapply", dbl_1e3 = tapply(dbl_1e3, fct_1e3, sum), mean_1e3 = tapply(df_1e3$x, df_1e3$g, mean),
      dbl_1e6 = tapply(dbl_1e6, fct_1e6, sum))
bench("taskCallbackManager", setup = { handlers <- list() }, scalar = taskCallbackManager(handlers))
bench("tcrossprod", setup = { v <- dbl_1e3[1:100] }, mat_10 = tcrossprod(mat_10), mat_100 = tcrossprod(mat_100),
      dbl_100 = tcrossprod(v))
skip("tempdir", "nondeterministic")
skip("tempfile", "nondeterministic")
bench("textConnection", txt_1e3 = { con <- textConnection(txt_1e3); close(con) },
      write = { con <- textConnection(NULL, "w"); writeLines(chr_1, con); close(con) })
bench("textConnectionValue", txt_1e3 = {
  con <- textConnection(NULL, "w")
  writeLines(txt_1e3, con)
  value <- textConnectionValue(con)
  close(con)
  value
})
bench("tolower", chr_1 = tolower(chr_1), chr_1e3 = tolower(chr_1e3), txt_1e3 = tolower(txt_1e3))
bench("topenv", setup = { env <- new.env() }, scalar = topenv(env), closure = topenv(environment(fun)))
bench("toString", int_1e3 = toString(int_1e3), chr_1e3 = toString(chr_1e3), small = toString(letters))
bench("toString.default", int_1e3 = toString.default(int_1e3))
bench("toupper", chr_1 = toupper(chr_1), chr_1e3 = toupper(chr_1e3), txt_1e3 = toupper(txt_1e3))
skip("trace", "changes the session")
bench(".traceback", setup = { calls <- list(quote(f(x)), quote(g(y, z = 1))) }, small = .traceback(calls))
bench("traceback", setup = { calls <- list(quote(f(x)), quote(g(y, z = 1))) }, small = traceback(calls))
skip("tracemem", "nondeterministic")
bench("tracingState", setup = { on <- NULL }, scalar = tracingState(on))
bench("transform", df_1e3 = transform(df_1e3, z = x + y), df_10 = transform(df_10, x = x * 2))
bench("transform.data.frame", df_1e3 = transform.data.frame(df_1e3, z = x + y))
bench("transform.default", setup = { columns <- list(a = dbl_1e3, b = int_1e3) },
      lst_1e3 = transform.default(columns, c = a + b))
bench("trigamma", dbl_1 = trigamma(dbl_1), pos_1e3 = trigamma(pos_1e3), pos_1e6 = trigamma(pos_1e6))
bench("trimws", setup = { padded <- paste0("  ", chr_1e3, " \t"); padded_1 <- padded[1L] }, chr_1 = trimws(padded_1),
      chr_1e3 = trimws(padded), left_1e3 = trimws(padded, "left"))
bench("trunc", dbl_1 = trunc(dbl_1), dbl_1e3 = trunc(dbl_1e3), dbl_1e6 = trunc(dbl_1e6))
bench("trunc.Date", date_1e3 = trunc.Date(date_1e3), months_1e3 = trunc.Date(date_1e3, units = "months"))
bench("trunc.POSIXt", time_1 = trunc.POSIXt(time_1, "hours"), time_1e3 = trunc.POSIXt(time_1e3, "days"))
skip("truncate", "writes files")
skip("truncate.connection", "writes files")
bench("try", scalar = try(fun(dbl_1)), error = try(stop(chr_1), silent = TRUE))
bench("tryCatch", scalar = tryCatch(fun(dbl_1), error = function(e) NULL),
      error = tryCatch(stop(chr_1), error = function(e) NULL),
      warning = tryCatch(warning(chr_1), warning = function(w) NULL),
      finally = tryCatch(fun(dbl_1), finally = dbl_1),
      handlers = tryCatch(fun(dbl_1), error = function(e) NULL, warning = function(w) NULL,
                          message = function(m) NULL))
bench("tryInvokeRestart", setup = { restart <- "rybenchRestart" }, scalar = tryInvokeRestart(restart),
      found = withRestarts(tryInvokeRestart("rybenchRestart", dbl_1), rybenchRestart = function(x) x))
bench(".tryResumeInterrupt", scalar = .tryResumeInterrupt())
bench("typeof", dbl_1 = typeof(dbl_1), lst_10 = typeof(lst_10), fun = typeof(fun))
bench("unCfillPOSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = unCfillPOSIXlt(lt))
bench("unclass", df_1e3 = unclass(df_1e3), fct_1e3 = unclass(fct_1e3), date_1e3 = unclass(date_1e3),
      dbl_1e6 = unclass(dbl_1e6))
skip("undebug", "changes the session")
bench("union", setup = { other <- rev(int_1e3) + 500L; chr_other <- rev(chr_1e3) }, int_1e3 = union(int_1e3, other),
      chr_1e3 = union(chr_1e3, chr_other), dbl_1e6 = union(dbl_1e6, dbl_1e6))
bench("unique", dbl_1e3 = unique(dbl_1e3), int_1e3 = unique(int_1e3), chr_1e3 = unique(chr_1e3),
      fct_1e3 = unique(fct_1e3), lst_1e3 = unique(lst_1e3), df_1e3 = unique(df_1e3),
      int_1e6 = unique(int_1e6), chr_1e6 = unique(chr_1e6))
bench("unique.array", setup = { cube <- array(int_1e3 %% 3L, c(10L, 10L, 10L)) }, int_1e3 = unique.array(cube))
bench("unique.data.frame", df_10 = unique.data.frame(df_10), df_1e3 = unique.data.frame(df_1e3))
bench("unique.default", int_1e3 = unique.default(int_1e3))
bench("unique.matrix", setup = { m <- imat_100 %% 2L }, imat_100 = unique.matrix(m))
bench("unique.numeric_version", setup = { versions <- numeric_version(sprintf("%d.%d", int_1e3 %% 5L, int_1e3 %% 13L)) },
      ver_1e3 = unique.numeric_version(versions))
bench("unique.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = unique.POSIXlt(lt))
bench("unique.warnings", setup = {
  warned <- structure(list(quote(f(x)), NULL, quote(f(x))), names = c("w1", "w2", "w1"), class = "warnings")
}, small = unique.warnings(warned))
bench("units", setup = { dt <- difftime(time_1e3, time_1, units = "days") }, time_1e3 = units(dt))
bench("units.difftime", setup = { dt <- difftime(time_1e3, time_1, units = "days") }, time_1e3 = units.difftime(dt))
bench("units<-", setup = { dt <- difftime(time_1e3, time_1, units = "days") },
      time_1e3 = { y <- dt; units(y) <- "hours" })
bench("units<-.difftime", setup = { dt <- difftime(time_1e3, time_1, units = "days") },
      time_1e3 = `units<-.difftime`(dt, value = "hours"))
skip("unix.time", "defunct")
skip("unlink", "writes files")
bench("unlist", setup = { nested <- split(dbl_1e3, fct_1e3) }, lst_10 = unlist(lst_10), lst_1e3 = unlist(lst_1e3),
      nested_1e3 = unlist(nested), unnamed_1e3 = unlist(nested, use.names = FALSE), lst_1e6 = unlist(lst_1e6))
skip("unloadNamespace", "changes the session")
bench("unlockBinding", setup = { env <- new.env(); assign("a", 1, envir = env); lockBinding("a", env); name <- "a" },
      scalar = unlockBinding(name, env))
bench("unname", setup = { m <- mat_100; dimnames(m) <- list(paste0("r", 1:100), paste0("c", 1:100)) },
      named_1e3 = unname(named_1e3), df_1e3 = unname(df_1e3), mat_100 = unname(m), named_1e6 = unname(named_1e6))
bench("unserialize", setup = { dbl_bytes <- serialize(dbl_1e3, NULL); lst_bytes <- serialize(lst_1e3, NULL);
  df_bytes <- serialize(df_1e3, NULL); dbl6_bytes <- serialize(dbl_1e6, NULL) },
  dbl_1e3 = unserialize(dbl_bytes), lst_1e3 = unserialize(lst_bytes), df_1e3 = unserialize(df_bytes),
  dbl_1e6 = unserialize(dbl6_bytes))
bench("unsplit", setup = { parts <- split(dbl_1e3, fct_1e3); df_parts <- split(df_1e3, df_1e3$g) },
      dbl_1e3 = unsplit(parts, fct_1e3), df_1e3 = unsplit(df_parts, df_1e3$g))
skip("untrace", "changes the session")
bench("untracemem", setup = { y <- dbl_1e3 }, scalar = untracemem(y))
skip("unz", "reads files")
bench("upper.tri", mat_100 = upper.tri(mat_100), diag_100 = upper.tri(mat_100, diag = TRUE),
      mat_1000_1e6 = upper.tri(mat_1000))
skip("url", "network")
skip("use", "changes the session")
bench("UseMethod", setup = {
  generic <- function(x) UseMethod("generic")
  generic.default <- function(x) x
  generic.rybench <- function(x) 1
  object <- structure(1, class = "rybench")
}, default = generic(dbl_1), class = generic(object))
bench("utf8ToInt", setup = { long <- paste(txt_1e3, collapse = " ") }, chr_1 = utf8ToInt(chr_1), txt_1e3 = utf8ToInt(long))
bench(".valid.factor", fct_1e3 = .valid.factor(fct_1e3))
bench("validEnc", chr_1e3 = validEnc(chr_1e3), chr_1e6 = validEnc(chr_1e6))
bench("validUTF8", chr_1e3 = validUTF8(chr_1e3), chr_1e6 = validUTF8(chr_1e6))
bench("vapply", setup = { pair <- function(x) c(x, x) }, lst_1e3 = vapply(lst_1e3, fun, 0),
      chr_1e3 = vapply(chr_1e3, nchar, 0L, USE.NAMES = FALSE), matrix_1e3 = vapply(lst_1e3, pair, numeric(2L)),
      lst_1e6 = vapply(lst_1e6, length, 0L))
bench("vector", setup = { n <- 1000L; n6 <- 1000000L }, list_1e3 = vector("list", n),
      numeric_1e3 = vector("numeric", n), character_1e3 = vector("character", n), numeric_1e6 = vector("numeric", n6))
bench("Vectorize", setup = { add <- function(a, b) a + b; vectorized <- Vectorize(add); x <- dbl_1e3[1:100] },
      scalar = Vectorize(add), call_100 = vectorized(x, dbl_1))
bench("warning", setup = { condition <- simpleWarning("rybench") }, chr_1 = suppressWarnings(warning(chr_1)),
      condition = suppressWarnings(warning(condition)))
bench("warningCondition", scalar = warningCondition(chr_1, class = "rybenchWarning"))
bench("warnings", setup = { f <- function(x) warnings() }, scalar = f(dbl_1))
bench("weekdays", date_1e3 = weekdays(date_1e3), time_1e3 = weekdays(time_1e3))
bench("weekdays.Date", date_1e3 = weekdays.Date(date_1e3), abbreviate_1e3 = weekdays.Date(date_1e3, TRUE))
bench("weekdays.POSIXt", time_1e3 = weekdays.POSIXt(time_1e3))
bench("which", lgl_1e3 = which(lgl_1e3), dbl_1e3 = which(dbl_1e3 > 0), lgl_1e6 = which(lgl_1e6))
bench("which.max", dbl_1e3 = which.max(dbl_1e3), int_1e3 = which.max(int_1e3), dbl_1e6 = which.max(dbl_1e6))
bench("which.min", dbl_1e3 = which.min(dbl_1e3), dbl_1e6 = which.min(dbl_1e6))
skip("while", "language construct")
bench("with", df_1e3 = with(df_1e3, x + y), lst_10 = with(lst_10, a + g))
bench("with.default", df_1e3 = with.default(df_1e3, x + y))
bench("withAutoprint", setup = { code <- expression(dbl_1 + 1, x <- 2) }, small = withAutoprint(code, evaluated = TRUE))
bench("withCallingHandlers", scalar = withCallingHandlers(fun(dbl_1), warning = function(w) NULL),
      muffled = withCallingHandlers(warning(chr_1), warning = function(w) invokeRestart("muffleWarning")),
      message = withCallingHandlers(message(chr_1), message = function(m) invokeRestart("muffleMessage")))
bench("within", df_1e3 = within(df_1e3, z <- x + y), lst_10 = within(lst_10, k <- a + g))
bench("within.data.frame", df_1e3 = within.data.frame(df_1e3, { z <- x + y; w <- z * 2 }))
bench("within.list", setup = { columns <- list(a = dbl_1e3, b = int_1e3) }, lst_1e3 = within.list(columns, c <- a + b))
bench("withRestarts", scalar = withRestarts(fun(dbl_1), rybenchRestart = function() NULL),
      invoked = withRestarts(invokeRestart("rybenchRestart", dbl_1), rybenchRestart = function(x) x))
bench("withVisible", scalar = withVisible(fun(dbl_1)), invisible = withVisible(invisible(dbl_1)))
bench("write", int_1e3 = write(int_1e3, ""), dbl_1e3 = write(dbl_1e3, "", ncolumns = 10L))
bench("write.dcf", df_10 = write.dcf(df_10, file = ""))
bench("writeBin", dbl_1e3 = writeBin(dbl_1e3, raw()), int_1e3 = writeBin(int_1e3, raw()),
      chr_1e3 = writeBin(chr_1e3, raw()), dbl_1e6 = writeBin(dbl_1e6, raw()))
bench("writeChar", chr_1 = writeChar(chr_1, raw()), txt_1e3 = writeChar(txt_1e3, raw()))
bench("writeLines", chr_1 = writeLines(chr_1), txt_1e3 = writeLines(txt_1e3),
      connection_1e3 = { con <- textConnection(NULL, "w"); writeLines(txt_1e3, con); close(con) })
bench("xor", setup = { other <- rev(lgl_1e3); other6 <- rev(lgl_1e6) }, lgl_1 = xor(lgl_1, lgl_1),
      lgl_1e3 = xor(lgl_1e3, other), lgl_1e6 = xor(lgl_1e6, other6))
bench("xpdrows.data.frame", setup = { columns <- unclass(df_10); old <- 1:10; new <- 11:20 },
      df_10 = xpdrows.data.frame(columns, old, new))
bench("xtfrm", dbl_1e3 = xtfrm(dbl_1e3), fct_1e3 = xtfrm(fct_1e3), date_1e3 = xtfrm(date_1e3))
bench("xtfrm.AsIs", setup = { y <- I(dbl_1e3) }, dbl_1e3 = xtfrm.AsIs(y))
skip("xtfrm.data.frame", "only stops with an error")
bench("xtfrm.Date", date_1e3 = xtfrm.Date(date_1e3))
bench("xtfrm.default", dbl_1e3 = xtfrm.default(dbl_1e3), int_1e3 = xtfrm.default(int_1e3))
bench("xtfrm.difftime", setup = { dt <- difftime(time_1e3, time_1, units = "days") }, time_1e3 = xtfrm.difftime(dt))
bench("xtfrm.factor", fct_1e3 = xtfrm.factor(fct_1e3))
bench("xtfrm.numeric_version", setup = { versions <- numeric_version(sprintf("%d.%d", int_1e3 %% 5L, int_1e3 %% 13L)) },
      ver_1e3 = xtfrm.numeric_version(versions))
bench("xtfrm.POSIXct", time_1e3 = xtfrm.POSIXct(time_1e3))
bench("xtfrm.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = xtfrm.POSIXlt(lt))
skip("xzfile", "reads files")
bench("zapsmall", dbl_1e3 = zapsmall(dbl_1e3), dbl_1e6 = zapsmall(dbl_1e6))
skip("zstdfile", "reads files")
skip("{", "language construct")
bench("|", setup = { other <- rev(lgl_1e3); other6 <- rev(lgl_1e6) }, lgl_1 = lgl_1 | lgl_1,
      lgl_1e3 = lgl_1e3 | other, lgl_1e6 = lgl_1e6 | other6)
bench("|.hexmode", setup = { h <- as.hexmode(int_1e3); mask <- as.hexmode(255L) }, int_1e3 = h | mask)
bench("|.octmode", setup = { o <- as.octmode(int_1e3); mask <- as.octmode(7L) }, int_1e3 = o | mask)
bench("||", lgl_1 = lgl_1 || lgl_1, short = !lgl_1 || dbl_1 > 0)
bench("~", scalar = y ~ x + g, one_sided = ~ dbl_1)
