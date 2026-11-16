skip("?", "interactive")
bench("adist",
  setup = { y <- "id01234_q"; x <- chr_1e3[1:40] },
  scalar = adist(chr_1, y),
  chr_1e3 = adist(chr_1e3, y),
  small = adist(x),
  partial = adist(y, txt_1e3[1:100], partial = TRUE))
bench("alarm", scalar = alarm())
bench("apropos", setup = { p <- "^read\\." }, scalar = apropos(p), mode = apropos(p, mode = "function"))
bench("aregexec",
  setup = { p <- "prise" },
  scalar = aregexec(p, txt_1, max.distance = 1),
  txt_1e3 = aregexec(p, txt_1e3, max.distance = 1))
bench("argsAnywhere", setup = { nm <- "head" }, scalar = do.call(argsAnywhere, list(nm)))
bench("as.person",
  setup = { s <- "Jane Doe <jane@example.org> [aut, cre]"; many <- paste(s, "and John Smith [ctb]") },
  scalar = as.person(s),
  small = as.person(many))
bench("as.personList", setup = { p <- person("Jane", "Doe") }, scalar = as.personList(p))
bench("as.relistable", scalar = as.relistable(lst_10), lst_1e3 = as.relistable(lst_1e3))
bench("as.roman",
  setup = { r <- as.character(as.roman(int_1e3)) },
  scalar = as.roman(int_1),
  int_1e3 = as.roman(int_1e3),
  int_1e6 = as.roman(int_1e6),
  chr_1e3 = as.roman(r))
bench("asDateBuilt", setup = { built <- "R 4.6.2; x86_64-pc-linux-gnu; 2026-11-11 08:11:10 UTC; unix" },
  scalar = asDateBuilt(built))
skip("askYesNo", "interactive")
skip("aspell", "needs an external tool")
skip("aspell_package_C_files", "needs an external tool")
skip("aspell_package_R_files", "needs an external tool")
skip("aspell_package_Rd_files", "needs an external tool")
skip("aspell_package_vignettes", "needs an external tool")
skip("aspell_write_personal_dictionary_file", "writes files")
skip("assignInMyNamespace", "changes the session")
skip("assignInNamespace", "changes the session")
bench(".AtNames",
  setup = { x <- methods::getClass("classRepresentation"); p <- "s" },
  scalar = .AtNames(x, p))
skip("available.packages", "network")
bench("bibentry",
  setup = { title <- "A Study of Yield Curves"; authors <- c(person("Jane", "Doe"), person("John", "Smith")) },
  scalar = bibentry(bibtype = "Article", title = title, author = authors, journal = "J. Finance",
                    year = 2020, volume = 12, pages = "1--20", key = "doe2020"))
bench("bitstring", scalar = bitstring(int_1), int_1e3 = bitstring(int_1e3), dbl_1e3 = bitstring(dbl_1e3))
skip("browseEnv", "interactive")
skip("browseURL", "interactive")
skip("browseVignettes", "interactive")
skip("bug.report", "interactive")
bench("capture.output",
  scalar = capture.output(print(dbl_1)),
  dbl_1e3 = capture.output(print(dbl_1e3)),
  df_10 = capture.output(print(df_10)),
  lst_10 = capture.output(str(lst_10)),
  cat_1e3 = capture.output(cat(chr_1e3, sep = "\n")))
bench("changedFiles",
  setup = {
    dir.create("snap", showWarnings = FALSE)
    for (path in file.path("snap", sprintf("f%02d.txt", 1:20))) writeLines(chr_1e3[1:10], path)
    before <- fileSnapshot("snap", md5sum = TRUE)
    writeLines("changed", file.path("snap", "f03.txt"))
    file.remove(file.path("snap", "f07.txt"))
    after <- fileSnapshot("snap", md5sum = TRUE)
  },
  small = changedFiles(before, after))
bench("charClass",
  setup = { s <- paste(txt_1e3, collapse = " "); codes <- utf8ToInt(s) },
  scalar = charClass(txt_1, "alpha"),
  txt_1e3 = charClass(s, "space"),
  int_1e3 = charClass(codes, "punct"))
skip("checkCRAN", "network")
bench(".checkHT", setup = { n <- 6L; d <- dim(mat_100) }, scalar = .checkHT(n, d))
skip("chooseBioCmirror", "interactive")
skip("chooseCRANmirror", "interactive")
skip("citation", "reads files")
bench("cite",
  setup = { bib <- bibentry(bibtype = "Manual", title = "T", author = person("Jane", "Doe"), year = 2020, key = "k1"); key <- "k1" },
  scalar = cite(key, bib))
bench("citeNatbib",
  setup = { bib <- bibentry(bibtype = "Manual", title = "T", author = person("Jane", "Doe"), year = 2020, key = "k1"); key <- "k1" },
  scalar = citeNatbib(key, bib))
bench("citEntry", setup = { title <- "A Manual" },
  scalar = citEntry(entry = "Manual", title = title, author = "Jane Doe", year = "2020", textVersion = "Doe (2020)"))
bench("citFooter", setup = { s <- "Please cite the paper." }, scalar = citFooter(s))
bench("citHeader", setup = { s <- "To cite this package use:" }, scalar = citHeader(s))
skip("close.socket", "network")
bench("clrhash",
  setup = { h <- hashtab(); for (k in 1:1000) sethash(h, k, k) },
  scalar = clrhash(h))
bench("combn",
  setup = { x <- seq_len(30L); y <- seq_len(12L) },
  small = combn(y, 3L),
  int_30 = combn(x, 3L),
  fun = combn(y, 4L, FUN = sum))
bench("compareVersion",
  setup = { a <- "1.10.2"; b <- "1.9.15" },
  scalar = compareVersion(a, b))
bench("contrib.url", setup = { repos <- c(CRAN = "https://cloud.r-project.org", extra = "https://example.org/r") },
  scalar = contrib.url(repos, type = "source"))
bench("count.fields",
  setup = { lines <- capture.output(write.csv(df_1e3, row.names = FALSE)); few <- lines[1:11] },
  small = { con <- textConnection(few); r <- count.fields(con, sep = ","); close(con); r },
  df_1e3 = { con <- textConnection(lines); r <- count.fields(con, sep = ","); close(con); r })
skip("create.post", "interactive")
skip("data", "reads files")
skip("data.entry", "interactive")
skip("dataentry", "interactive")
skip("de", "interactive")
bench("de.ncols", setup = { x <- list(a = 1:3, b = letters, m = mat_10) }, small = de.ncols(x))
bench("de.restore",
  setup = { x <- list(a = 1:3, b = letters, m = mat_10); nc <- de.ncols(x); dl <- de.setup(x, names(x), nc[, 1L]) },
  small = de.restore(dl, nc[, 1L], nc[, 2L], names(x), x))
bench("de.setup",
  setup = { x <- list(a = 1:3, b = letters, m = mat_10); nc <- de.ncols(x)[, 1L] },
  small = de.setup(x, names(x), nc))
skip("debugcall", "interactive")
skip("debugger", "interactive")
skip("demo", "reads files")
bench(".DollarNames",
  setup = { p <- "v00" },
  lst_10 = .DollarNames(lst_10, ""),
  env_1e3 = .DollarNames(env_1e3, p),
  df_1e3 = .DollarNames(df_1e3, ""))
skip("download.file", "network")
skip("download.packages", "network")
bench("dump.frames", setup = { name <- "sweep_dump" }, scalar = dump.frames(name, to.file = FALSE))
skip("edit", "interactive")
skip("emacs", "interactive")
skip("example", "reads files")
skip("file.edit", "interactive")
skip("file_test", "reads files")
skip("fileSnapshot", "reads files")
bench("find", setup = { nm <- "head" }, scalar = find(nm), mode = find(nm, mode = "function"))
bench("findCRANmirror", setup = { type <- "web" }, scalar = findCRANmirror(type))
bench("findLineNum",
  setup = {
    src <- srcfilecopy("sweep.R", c("f <- function(x) {", "  y <- x + 1", "  y * 2", "}"))
    e <- new.env()
    eval(parse(text = src$lines, srcfile = src, keep.source = TRUE), e)
    loc <- "sweep.R#2"
  },
  small = findLineNum(loc, envir = e, lastenv = e))
bench("findMatches",
  setup = { p <- "id01" },
  scalar = findMatches(p, chr_1),
  chr_1e3 = findMatches(p, chr_1e3))
skip("fix", "interactive")
skip("fixInNamespace", "interactive")
bench("flush.console", scalar = flush.console())
bench("formatOL",
  setup = { x <- chr_1e3[1:10] },
  small = formatOL(x),
  roman = formatOL(x, type = "i"),
  chr_1e3 = formatOL(chr_1e3))
bench("formatUL", setup = { x <- chr_1e3[1:10] }, small = formatUL(x), chr_1e3 = formatUL(chr_1e3))
bench("getAnywhere", setup = { nm <- "format.Date"; nm2 <- "head" }, method = getAnywhere(nm), scalar = getAnywhere(nm2))
skip("getCRANmirrors", "reads files")
bench("getFromNamespace", setup = { nm <- "head.matrix"; ns <- "utils" }, scalar = getFromNamespace(nm, ns))
bench("gethash",
  setup = {
    h <- hashtab(); for (k in 1:1000) sethash(h, k, k * 2)
    hc <- hashtab(); for (k in chr_1e3) sethash(hc, k, nchar(k))
  },
  scalar = gethash(h, int_1),
  chr = gethash(hc, chr_1),
  miss = gethash(h, dbl_1, nomatch = NA),
  int_1e3 = lapply(int_1e3, gethash, h = h))
bench("getParseData",
  setup = {
    small <- parse(text = "f <- function(x, y = 2) x + y * 3", keep.source = TRUE)
    big <- parse(text = deparse(utils::write.table), keep.source = TRUE)
  },
  small = getParseData(small),
  large = getParseData(big))
bench("getParseText",
  setup = {
    p <- parse(text = deparse(utils::write.table), keep.source = TRUE)
    pd <- getParseData(p)
    id <- pd$id[pd$parent == 0L]
    ids <- pd$id[pd$token == "SYMBOL"]
  },
  scalar = getParseText(pd, id),
  many = getParseText(pd, ids))
bench("getS3method", setup = { generic <- "print"; class <- "data.frame" }, scalar = getS3method(generic, class))
bench("getSrcDirectory",
  setup = { f <- eval(parse(text = "function(x) x + 1", srcfile = srcfilecopy("/tmp/dir/sweep.R", "function(x) x + 1"), keep.source = TRUE)[[1L]]) },
  scalar = getSrcDirectory(f))
bench("getSrcFilename",
  setup = { f <- eval(parse(text = "function(x) x + 1", srcfile = srcfilecopy("/tmp/dir/sweep.R", "function(x) x + 1"), keep.source = TRUE)[[1L]]) },
  scalar = getSrcFilename(f, full.names = TRUE))
bench("getSrcLocation",
  setup = { f <- eval(parse(text = "function(x) x + 1", srcfile = srcfilecopy("/tmp/dir/sweep.R", "function(x) x + 1"), keep.source = TRUE)[[1L]]) },
  scalar = getSrcLocation(f, "line"))
bench("getSrcref",
  setup = { f <- eval(parse(text = "function(x) x + 1", srcfile = srcfilecopy("/tmp/dir/sweep.R", "function(x) x + 1"), keep.source = TRUE)[[1L]]) },
  scalar = getSrcref(f))
bench("getTxtProgressBar", setup = { pb <- txtProgressBar(0, 100, style = 3); setTxtProgressBar(pb, 40) },
  scalar = getTxtProgressBar(pb))
bench("glob2rx",
  setup = { p <- "*.R"; pats <- paste0("*", chr_1e3, "?.R") },
  scalar = glob2rx(p),
  chr_1e3 = glob2rx(pats))
bench("globalVariables", setup = { e <- new.env(); vars <- c("x", "y", "g") }, scalar = globalVariables(vars, package = e))
bench("hashtab", setup = { type <- "identical"; size <- 1000L }, scalar = hashtab(type), sized = hashtab(type, size))
bench("hasName",
  setup = { nms <- c("x", "zz", "g") },
  scalar = hasName(df_1e3, "x"),
  small = hasName(lst_10, nms),
  named_1e3 = hasName(named_1e3, nms))
bench("head",
  scalar = head(dbl_1),
  dbl_1e3 = head(dbl_1e3),
  dbl_1e6 = head(dbl_1e6),
  neg_1e6 = head(dbl_1e6, -10L),
  chr_1e3 = head(chr_1e3, 20L),
  fct_1e3 = head(fct_1e3),
  lst_1e3 = head(lst_1e3),
  df_10 = head(df_10, 3L),
  df_1e3 = head(df_1e3),
  df_1e6 = head(df_1e6),
  mat_1000 = head(mat_1000),
  mat_cols = head(mat_100, c(5L, 5L)))
bench("head.matrix",
  setup = { n <- 10L },
  mat_100 = head.matrix(mat_100, n),
  mat_1000 = head.matrix(mat_1000, n),
  keepnums = head.matrix(mat_1000, -n, keepnums = TRUE))
skip("help", "interactive")
skip("help.request", "interactive")
skip("help.search", "interactive")
skip("help.start", "interactive")
skip("history", "interactive")
skip("hsearch_db", "reads files")
skip("hsearch_db_concepts", "reads files")
skip("hsearch_db_keywords", "reads files")
skip("install.packages", "changes the session")
skip("installed.packages", "reads files")
bench("is.hashtab", setup = { h <- hashtab() }, scalar = is.hashtab(h), other = is.hashtab(lst_10))
bench("is.relistable", setup = { x <- as.relistable(lst_10) }, scalar = is.relistable(x), other = is.relistable(lst_10))
bench("isS3method", setup = { nm <- "print.data.frame"; nm2 <- "all.equal" }, scalar = isS3method(nm), generic = isS3method(nm2))
bench("isS3stdGeneric", scalar = isS3stdGeneric(print), other = isS3stdGeneric(fun))
bench("limitedLabels",
  setup = { calls <- rep(list(quote(f(x, y = 2)), quote(g(lapply(z, function(v) v + 1)))), 20L) },
  small = limitedLabels(calls))
skip("loadhistory", "reads files")
bench("localeToCharset", setup = { loc <- "en_US.UTF-8"; loc2 <- "de_DE@euro" }, scalar = localeToCharset(loc), latin = localeToCharset(loc2))
bench("ls.str",
  setup = { small <- list2env(lst_10) },
  small = ls.str(small),
  env_1e3 = ls.str(env_1e3),
  print = print(ls.str(small)))
bench("lsf.str",
  setup = { e <- list2env(list(f = fun, g = sum, x = 1, h = function(a, b) a)); ns <- asNamespace("stats") },
  small = lsf.str(e),
  stats = lsf.str(envir = ns),
  print = print(lsf.str(e)))
skip("maintainer", "reads files")
skip("make.packages.html", "writes files")
skip("make.socket", "network")
bench("makeRweaveLatexCodeRunner", setup = { evaluate <- RweaveEvalWithOpt }, scalar = makeRweaveLatexCodeRunner(evaluate))
bench("maphash",
  setup = { h <- hashtab(); for (k in 1:1000) sethash(h, k, k); total <- 0; add <- function(k, v) total <<- total + v },
  int_1e3 = maphash(h, add))
skip("memory.limit", "defunct")
skip("memory.size", "defunct")
skip("menu", "interactive")
bench("methods",
  setup = { generic <- "summary"; class <- "data.frame" },
  generic = methods(generic),
  class = methods(class = class))
skip("mirror2html", "writes files")
bench("modifyList",
  setup = {
    v <- list(a = 10, z = "new", f = NULL)
    x <- as.list(named_1e3); val <- x[seq(1L, 1000L, by = 10L)]
    nested <- list(a = list(b = list(c = 1, d = 2), e = 3), f = 4); nval <- list(a = list(b = list(c = 9)), f = 5)
  },
  small = modifyList(lst_10, v),
  nested = modifyList(nested, nval),
  lst_1e3 = modifyList(x, val))
skip("new.packages", "network")
skip("news", "reads files")
skip("nsl", "network")
bench("numhash", setup = { h <- hashtab(); for (k in 1:1000) sethash(h, k, k) }, scalar = numhash(h))
bench("object.size",
  scalar = object.size(dbl_1),
  dbl_1e6 = object.size(dbl_1e6),
  chr_1e3 = object.size(chr_1e3),
  lst_1e3 = object.size(lst_1e3),
  df_1e3 = object.size(df_1e3),
  chr_1e6 = object.size(chr_1e6))
skip("old.packages", "network")
skip("package.skeleton", "writes files")
skip("packageDate", "reads files")
skip("packageDescription", "reads files")
bench("packageName", setup = { ns <- asNamespace("stats") }, scalar = packageName(ns))
skip("packageStatus", "network")
skip("packageVersion", "reads files")
skip("page", "interactive")
bench("person",
  setup = { given <- "Jane"; family <- "Doe" },
  scalar = person(given, family),
  full = person(given, family, email = "jane@example.org", role = c("aut", "cre"), comment = c(ORCID = "0000-0001-2345-6789")))
bench("personList", setup = { a <- person("Jane", "Doe"); b <- person("John", "Smith") }, scalar = personList(a, b))
skip("pico", "interactive")
bench("process.events", scalar = process.events())
bench("prompt", setup = { f <- function(x, y = 2, ...) x + y }, scalar = prompt(f, filename = NA, name = "f"))
bench("promptData", scalar = promptData(df_10, filename = NA, name = "df_10"))
bench("promptImport", setup = { f <- utils::head },
  scalar = promptImport(f, filename = NA, name = "head", importedFrom = "utils"))
skip("promptPackage", "reads files")
bench("rc.getOption", setup = { nm <- "funarg.suffix" }, scalar = rc.getOption(nm))
bench("rc.options", setup = { nm <- "funarg.suffix" }, scalar = rc.options(nm))
bench("rc.settings", setup = { current <- rc.status()$settings$ipck }, scalar = rc.settings(ipck = current))
bench("rc.status", scalar = rc.status())
bench("read.csv",
  setup = {
    small <- capture.output(write.csv(df_10, row.names = FALSE))
    lines <- capture.output(write.csv(df_1e3, row.names = FALSE))
    large <- capture.output(write.csv(df_1e6[1:1e5, ], row.names = FALSE))
    classes <- c("integer", "numeric", "numeric", "factor", "character", "logical")
  },
  small = read.csv(text = small),
  df_1e3 = read.csv(text = lines),
  classes = read.csv(text = lines, colClasses = classes),
  df_1e5 = read.csv(text = large))
bench("read.csv2",
  setup = { lines <- capture.output(write.csv2(df_1e3, row.names = FALSE)) },
  df_1e3 = read.csv2(text = lines))
bench("read.delim",
  setup = { lines <- capture.output(write.table(df_1e3, sep = "\t", row.names = FALSE, quote = FALSE)) },
  df_1e3 = read.delim(text = lines))
bench("read.delim2",
  setup = { lines <- capture.output(write.table(df_1e3, sep = "\t", dec = ",", row.names = FALSE, quote = FALSE)) },
  df_1e3 = read.delim2(text = lines))
bench("read.DIF",
  setup = {
    cell <- function(v) if (is.numeric(v)) c(sprintf("0,%s", format(v, digits = 15)), "V") else c("1,0", paste0("\"", v, "\""))
    rows <- lapply(seq_len(200L), function(i) c("-1,0", "BOT", cell(df_1e3$x[i]), cell(df_1e3$y[i]), cell(df_1e3$s[i])))
    lines <- c("TABLE", "0,1", "\"\"", "VECTORS", "0,3", "\"\"", "TUPLES", "0,200", "\"\"",
               "DATA", "0,0", "\"\"", unlist(rows), "-1,0", "EOD")
  },
  small = { con <- textConnection(lines); r <- read.DIF(con); close(con); r })
bench("read.fortran",
  setup = { lines <- sprintf("%5d%10.3f%10.3f", df_1e3$id, df_1e3$x, df_1e3$y); fmt <- c("I5", "2F10.3") },
  df_1e3 = { con <- textConnection(lines); r <- read.fortran(con, fmt); close(con); r })
bench("read.fwf",
  setup = { lines <- sprintf("%5d%10.3f%10.3f", df_1e3$id, df_1e3$x, df_1e3$y); widths <- c(5L, 10L, 10L) },
  df_1e3 = { con <- textConnection(lines); r <- read.fwf(con, widths); close(con); r })
skip("read.socket", "network")
bench("read.table",
  setup = {
    small <- capture.output(write.table(df_10))
    lines <- capture.output(write.table(df_1e3))
    plain <- capture.output(write.table(df_1e3, row.names = FALSE, col.names = FALSE, quote = FALSE))
    nums <- capture.output(write.table(mat_100, row.names = FALSE, col.names = FALSE))
    large <- capture.output(write.table(df_1e6[1:1e5, ], row.names = FALSE))
  },
  small = read.table(text = small),
  df_1e3 = read.table(text = lines),
  plain = read.table(text = plain, colClasses = c("integer", "numeric", "numeric", "character", "character", "logical")),
  mat_100 = read.table(text = nums),
  df_1e5 = read.table(text = large, header = TRUE))
skip("readCitationFile", "reads files")
skip("recover", "interactive")
bench("relist",
  setup = {
    sk <- as.relistable(list(a = 1, b = list(c = 2:4, d = 5), e = c(6, 7)))
    fl <- unlist(sk)
    big <- as.relistable(split(dbl_1e3, rep(1:100, each = 10L)))
    bigfl <- unlist(big)
  },
  small = relist(fl, sk),
  dbl_1e3 = relist(bigfl, big))
bench("remhash",
  setup = { h <- hashtab(); for (k in 1:1000) sethash(h, k, k) },
  scalar = remhash(h, int_1))
skip("remove.packages", "changes the session")
bench("removeSource",
  setup = { f <- eval(parse(text = deparse(utils::write.table), keep.source = TRUE)[[1L]]) },
  scalar = removeSource(f))
skip("Rprof", "writes files")
skip("Rprofmem", "writes files")
skip("RShowDoc", "interactive")
skip("RSiteSearch", "network")
skip("rtags", "reads files")
bench("Rtangle", scalar = Rtangle())
bench(".RtangleCodeLabel",
  setup = { chunk <- structure(c("#line 12 \"doc.Rnw\"", "x <- 1", "y <- x + 1"), srclines = 12:14) },
  scalar = .RtangleCodeLabel(chunk))
bench("RtangleFinish",
  setup = { object <- RtangleSetup("doc.Rnw", SweaveSyntaxNoweb, output = "stdout", quiet = TRUE) },
  scalar = RtangleFinish(object))
bench("RtangleOptions",
  setup = { options <- RtangleSetup("doc.Rnw", SweaveSyntaxNoweb, output = "stdout", quiet = TRUE)$options },
  scalar = RtangleOptions(options))
bench("RtangleRuncode",
  setup = {
    object <- RtangleSetup("doc.Rnw", SweaveSyntaxNoweb, output = "stdout", quiet = TRUE)
    options <- object$options; options$chunknr <- 1L; options$label <- "setup"
    chunk <- c("x <- 1", "y <- x + 1")
  },
  scalar = RtangleRuncode(object, chunk, options))
bench("RtangleSetup", setup = { file <- "doc.Rnw" },
  scalar = RtangleSetup(file, SweaveSyntaxNoweb, output = "stdout", quiet = TRUE))
bench("RtangleWritedoc",
  setup = {
    object <- RtangleSetup("doc.Rnw", SweaveSyntaxNoweb, output = "stdout", quiet = TRUE)
    chunk <- c("Some text.", "\\SweaveOpts{eval=FALSE}", "More text.")
  },
  scalar = RtangleWritedoc(object, chunk))
bench("RweaveChunkPrefix",
  setup = { options <- list(label = "fig1", prefix = TRUE, prefix.string = "doc", chunknr = 3L); unlabelled <- options[-1L] },
  scalar = RweaveChunkPrefix(options),
  unlabelled = RweaveChunkPrefix(unlabelled))
bench("RweaveEvalWithOpt",
  setup = { expr <- quote(sum(seq_len(100L))); options <- list(eval = TRUE, print = FALSE, term = TRUE) },
  scalar = RweaveEvalWithOpt(expr, options))
bench("RweaveLatex", scalar = RweaveLatex())
skip("RweaveLatexFinish", "writes files")
bench("RweaveLatexOptions",
  setup = {
    object <- RweaveLatexSetup(structure("doc.Rnw", encoding = "UTF-8"), SweaveSyntaxNoweb, output = "doc.tex", quiet = TRUE)
    close(object$output)
    options <- object$options
  },
  scalar = RweaveLatexOptions(options))
skip("RweaveLatexSetup", "writes files")
bench("RweaveLatexWritedoc",
  setup = {
    object <- RweaveLatexSetup(structure("doc.Rnw", encoding = "UTF-8"), SweaveSyntaxNoweb, output = "doc.tex", quiet = TRUE)
    close(object$output)
    object$output <- stdout()
    chunk <- structure(c("\\documentclass{article}", "\\begin{document}", "The value is \\Sexpr{1 + 1}.", "\\end{document}"),
                       srclines = 1:4, srcFilenum = rep(1L, 4L))
  },
  scalar = RweaveLatexWritedoc(object, chunk))
bench("RweaveTryStop", setup = { value <- 1; options <- list(chunknr = 1L) }, scalar = RweaveTryStop(value, options))
bench(".S3methods",
  setup = { generic <- "summary"; class <- "data.frame" },
  generic = .S3methods(generic),
  class = .S3methods(class = class))
skip("savehistory", "writes files")
skip("select.list", "interactive")
skip("sessionInfo", "reads files")
skip("setBreakpoint", "changes the session")
bench("sethash",
  setup = { h <- hashtab(); for (k in 1:1000) sethash(h, k, k); hc <- hashtab() },
  scalar = sethash(h, int_1, dbl_1),
  chr = sethash(hc, chr_1, dbl_1),
  int_1e3 = { for (k in int_1e3) sethash(h, k, k); numhash(h) })
skip("setRepositories", "interactive")
bench("setTxtProgressBar",
  setup = { pb <- txtProgressBar(0, 100, style = 3); quiet <- txtProgressBar(0, 100, style = 1) },
  scalar = setTxtProgressBar(pb, int_1),
  style1 = setTxtProgressBar(quiet, int_1))
bench("stack",
  setup = { small <- list(a = 1:3, b = 4:6, c = 7:9); x <- split(dbl_1e3, fct_1e3); df <- as.data.frame(mat_100) },
  small = stack(small),
  lst_1e3 = stack(x),
  df_100 = stack(df))
skip("Stangle", "writes files")
bench("str",
  scalar = str(dbl_1),
  dbl_1e3 = str(dbl_1e3),
  dbl_1e6 = str(dbl_1e6),
  int_1e3 = str(int_1e3),
  chr_1e3 = str(chr_1e3),
  chr_1e6 = str(chr_1e6),
  fct_1e3 = str(fct_1e3),
  lgl_1e3 = str(lgl_1e3),
  date_1e3 = str(date_1e3),
  time_1e3 = str(time_1e3),
  lst_10 = str(lst_10),
  lst_1e3 = str(lst_1e3),
  df_10 = str(df_10),
  df_1e3 = str(df_1e3),
  df_1e6 = str(df_1e6),
  mat_100 = str(mat_100),
  fun = str(fun),
  ts_1e3 = str(ts_1e3),
  nested = str(list(a = lst_10, b = list(c = df_10, d = fct_1e3))))
bench("strcapture",
  setup = { pattern <- "id([0-9]+)_([a-z])"; proto <- data.frame(num = integer(), letter = character()) },
  scalar = strcapture(pattern, chr_1, proto),
  chr_1e3 = strcapture(pattern, chr_1e3, proto))
bench("strOptions", setup = { width <- 60L }, scalar = strOptions(strict.width = "cut", vec.len = width %/% 15L))
skip("summaryRprof", "reads files")
bench("suppressForeignCheck", setup = { e <- new.env(); vars <- c("x", "y") }, scalar = suppressForeignCheck(vars, package = e))
skip("Sweave", "writes files")
skip("SweaveGetSourceName", "internal")
bench("SweaveHooks",
  setup = { options(SweaveHooks = list(fig = function() NULL, other = function() NULL)); opts <- list(fig = TRUE, other = FALSE) },
  scalar = SweaveHooks(opts),
  run = SweaveHooks(opts, run = TRUE))
skip("SweaveSyntConv", "writes files")
bench("tail",
  scalar = tail(dbl_1),
  dbl_1e3 = tail(dbl_1e3),
  dbl_1e6 = tail(dbl_1e6),
  neg_1e6 = tail(dbl_1e6, -10L),
  chr_1e3 = tail(chr_1e3, 20L),
  lst_1e3 = tail(lst_1e3),
  df_10 = tail(df_10, 3L),
  df_1e3 = tail(df_1e3),
  df_1e6 = tail(df_1e6),
  mat_1000 = tail(mat_1000))
bench("tail.matrix",
  setup = { n <- 10L },
  mat_100 = tail.matrix(mat_100, n),
  mat_1000 = tail.matrix(mat_1000, n),
  nokeep = tail.matrix(mat_1000, n, keepnums = FALSE))
skip("tar", "writes files")
bench("timestamp", setup = { stamp <- "##------ Mon Nov 16 12:00:00 2026 ------##" }, scalar = timestamp(stamp, quiet = TRUE))
bench("toBibtex",
  setup = {
    bib <- bibentry(bibtype = "Article", title = "A Study", author = c(person("Jane", "Doe"), person("John", "Smith")),
                    journal = "J. Finance", year = 2020, key = "doe2020")
    p <- person("Jane", "Doe", email = "jane@example.org")
  },
  bibentry = toBibtex(bib),
  person = toBibtex(p))
bench("toLatex",
  setup = {
    info <- structure(list(R.version = list(version.string = "R version 4.6.2", platform = "x86_64-pc-linux-gnu"),
      locale = "LC_CTYPE=en_US.UTF-8;LC_COLLATE=C", tzone = "UTC", tzcode_type = "system (glibc)", running = "Linux",
      RNGkind = c("Mersenne-Twister", "Inversion", "Rejection"), basePkgs = c("stats", "utils", "methods", "base"),
      matprod = "default", BLAS = "libRblas.so", LAPACK = "libRlapack.so", LA_version = "3.12.0"), class = "sessionInfo")
  },
  scalar = toLatex(info))
bench("txtProgressBar", setup = { max <- 100 }, scalar = txtProgressBar(0, max, style = 3), style1 = txtProgressBar(0, max))
bench("type.convert",
  setup = {
    chrdf <- data.frame(a = numstr_1e3, b = chr_1e3, c = as.character(int_1e3))
    intstr <- as.character(int_1e3); lglstr <- as.character(lgl_1e3)
  },
  scalar = type.convert(numstr_1e3[1L], as.is = TRUE),
  numstr_1e3 = type.convert(numstr_1e3, as.is = TRUE),
  numstr_1e6 = type.convert(numstr_1e6, as.is = TRUE),
  int_1e3 = type.convert(intstr, as.is = TRUE),
  lgl_1e3 = type.convert(lglstr, as.is = TRUE),
  chr_1e3 = type.convert(chr_1e3, as.is = FALSE),
  df_1e3 = type.convert(chrdf, as.is = TRUE),
  lst_10 = type.convert(lst_10, as.is = TRUE))
bench("typhash", setup = { h <- hashtab() }, scalar = typhash(h))
skip("undebugcall", "interactive")
bench("unstack",
  setup = { small <- stack(list(a = 1:3, b = 4:6)); x <- stack(split(dbl_1e3, rep(letters[1:10], 100L))) },
  small = unstack(small),
  dbl_1e3 = unstack(x))
skip("untar", "writes files")
skip("unzip", "writes files")
skip("update.packages", "network")
skip("upgrade", "network")
skip("url.show", "network")
bench("URLdecode",
  setup = { encoded <- vapply(txt_1e3, URLencode, "", USE.NAMES = FALSE); one <- encoded[1L] },
  scalar = URLdecode(one),
  txt_1e3 = URLdecode(encoded))
bench("URLencode",
  setup = { url <- "https://example.org/a path/q?x=1&y=two words" },
  scalar = URLencode(url),
  reserved = URLencode(url, reserved = TRUE),
  txt_1e3 = URLencode(txt_1e3))
skip("vi", "interactive")
skip("View", "interactive")
skip("vignette", "reads files")
bench("warnErrList",
  setup = { x <- lapply(1:100, function(i) if (i %% 10L == 0L) simpleError(paste("failed", i %% 3L)) else i) },
  small = warnErrList(x, warn = FALSE))
bench("write.csv",
  df_10 = write.csv(df_10),
  df_1e3 = write.csv(df_1e3),
  mat_100 = write.csv(mat_100, row.names = FALSE))
bench("write.csv2", df_1e3 = write.csv2(df_1e3))
skip("write.socket", "network")
bench("write.table",
  setup = { large <- df_1e6[1:1e5, ] },
  df_10 = write.table(df_10),
  df_1e3 = write.table(df_1e3),
  tsv = write.table(df_1e3, sep = "\t", quote = FALSE, row.names = FALSE),
  mat_100 = write.table(mat_100),
  df_1e5 = write.table(large, row.names = FALSE))
skip("xedit", "interactive")
skip("xemacs", "interactive")
skip("zip", "writes files")
