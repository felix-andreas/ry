bench("!", scalar = !lgl_1, lgl_1e3 = !lgl_1e3, lgl_1e6 = !lgl_1e6, int_1e3 = !int_1e3)
bench("!.hexmode", setup = { h <- as.hexmode(int_1e3) }, int_1e3 = `!.hexmode`(h))
bench("!.octmode", setup = { o <- as.octmode(int_1e3) }, int_1e3 = `!.octmode`(o))
bench("!=", scalar = dbl_1 != 0.5, dbl_1e3 = dbl_1e3 != sorted_1e3, dbl_1e6 = dbl_1e6 != sorted_1e6,
      int_1e6 = int_1e6 != 500L, chr_1e3 = chr_1e3 != chr_1)
bench("$", lst_10 = lst_10$g, df_1e3 = df_1e3$x, env_1e3 = env_1e3$v0500)
bench("$.DLLInfo", setup = { dll <- getLoadedDLLs()[["stats"]] }, small = `$.DLLInfo`(dll, "kmeans_Lloyd"))
bench("$.package_version", setup = { v <- package_version("4.6.2") }, scalar = `$.package_version`(v, "minor"))
bench("$<-", setup = { y <- lst_10; e <- new.env() }, lst_10 = y$a <- dbl_1, env = e$a <- dbl_1)
bench("$<-.data.frame", setup = { y <- df_1e3; z <- df_1e6 }, df_1e3 = y$z <- dbl_1e3, df_1e6 = z$z <- dbl_1e6)
bench("$<-.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3); m <- lt$min }, time_1e3 = lt$min <- m)
bench("%%", scalar = dbl_1 %% 0.3, dbl_1e3 = dbl_1e3 %% 7, dbl_1e6 = dbl_1e6 %% 7, int_1e6 = int_1e6 %% 7L)
bench("%*%", setup = { v <- dbl_1e3 }, mat_10 = mat_10 %*% mat_10, mat_100 = mat_100 %*% mat_100,
      dbl_1e3 = dbl_1e3 %*% v, mat_1000_vec = mat_1000 %*% v)
bench("%/%", scalar = dbl_1 %/% 0.3, dbl_1e3 = dbl_1e3 %/% 7, dbl_1e6 = dbl_1e6 %/% 7, int_1e6 = int_1e6 %/% 7L)
bench("%in%", setup = { table <- chr_1e3[1:100]; ints <- 1:100 }, scalar = chr_1 %in% table,
      chr_1e3 = chr_1e3 %in% table, int_1e3 = int_1e3 %in% ints, int_1e6 = int_1e6 %in% ints,
      chr_1e6 = chr_1e6 %in% chr_1e3)
bench("%notin%", setup = { table <- chr_1e3[1:100]; ints <- 1:100 }, scalar = chr_1 %notin% table,
      chr_1e3 = chr_1e3 %notin% table, int_1e6 = int_1e6 %notin% ints)
bench("%o%", setup = { x <- dbl_1e3[1:100] }, small = x %o% x, dbl_1e3 = dbl_1e3 %o% dbl_1e3)
bench("%x%", setup = { small <- mat_10[1:3, 1:3] }, small = small %x% small, mat_10 = mat_10 %x% mat_10)
bench("%||%", setup = { none <- NULL }, null = none %||% dbl_1, scalar = dbl_1 %||% none)
bench("&", scalar = lgl_1 & lgl_1, lgl_1e3 = lgl_1e3 & lgl_1e3, lgl_1e6 = lgl_1e6 & lgl_1e6)
bench("&&", setup = { other <- FALSE }, scalar = lgl_1 && other)
bench("&.hexmode", setup = { h <- as.hexmode(int_1e3) }, int_1e3 = `&.hexmode`(h, h))
bench("&.octmode", setup = { o <- as.octmode(int_1e3) }, int_1e3 = `&.octmode`(o, o))
skip("(", "language construct")
bench("*", scalar = dbl_1 * 2, dbl_1e3 = dbl_1e3 * dbl_1e3, dbl_1e6 = dbl_1e6 * dbl_1e6,
      int_1e6 = int_1e6 * 2L, mat_100 = mat_100 * mat_100, cpl_1e3 = cpl_1e3 * cpl_1e3)
bench("*.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins") }, dbl_1e3 = `*.difftime`(d, 2))
bench("+", scalar = dbl_1 + 1, dbl_1e3 = dbl_1e3 + dbl_1e3, dbl_1e6 = dbl_1e6 + dbl_1e6,
      int_1e6 = int_1e6 + 1L, mixed_1e6 = int_1e6 + dbl_1e6, mat_100 = mat_100 + mat_100)
bench("+.Date", scalar = `+.Date`(date_1, 1L), date_1e3 = `+.Date`(date_1e3, int_1e3),
      date_1e6 = `+.Date`(date_1e6, 1L))
bench("+.POSIXt", scalar = `+.POSIXt`(time_1, 60), time_1e3 = `+.POSIXt`(time_1e3, dbl_1e3),
      time_1e6 = `+.POSIXt`(time_1e6, 60))
bench("-", scalar = dbl_1 - 1, unary = -dbl_1e3, dbl_1e3 = dbl_1e3 - dbl_1e3, dbl_1e6 = dbl_1e6 - dbl_1e6,
      int_1e6 = int_1e6 - 1L)
bench("-.Date", scalar = `-.Date`(date_1, 1L), date_1e3 = `-.Date`(date_1e3, date_1e3),
      date_1e6 = `-.Date`(date_1e6, 1L))
bench("-.POSIXt", scalar = `-.POSIXt`(time_1, 60), time_1e3 = `-.POSIXt`(time_1e3, time_1e3),
      time_1e6 = `-.POSIXt`(time_1e6, 60))
bench("/", scalar = dbl_1 / 3, dbl_1e3 = dbl_1e3 / pos_1e3, dbl_1e6 = dbl_1e6 / pos_1e6, int_1e6 = int_1e6 / 2L)
bench("/.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins") }, dbl_1e3 = `/.difftime`(d, 2))
bench(":", setup = { small <- 10L; n <- 1000L; big <- 1000000L }, small = 1L:small, int_1e3 = 1L:n,
      int_1e6 = 1L:big, dbl = dbl_1:n)
bench("::", base = base::abs, stats = stats::median)
bench(":::", stats = stats:::median.default)
bench("<", scalar = dbl_1 < 0.5, dbl_1e3 = dbl_1e3 < sorted_1e3, dbl_1e6 = dbl_1e6 < sorted_1e6,
      int_1e6 = int_1e6 < 500L, chr_1e3 = chr_1e3 < chr_1)
skip("<-", "language construct")
skip("<<-", "language construct")
bench("<=", scalar = dbl_1 <= 0.5, dbl_1e3 = dbl_1e3 <= sorted_1e3, dbl_1e6 = dbl_1e6 <= sorted_1e6,
      int_1e6 = int_1e6 <= 500L)
skip("=", "language construct")
bench("==", scalar = dbl_1 == 0.5, dbl_1e3 = dbl_1e3 == sorted_1e3, dbl_1e6 = dbl_1e6 == sorted_1e6,
      int_1e6 = int_1e6 == 500L, chr_1e3 = chr_1e3 == chr_1, chr_1e6 = chr_1e6 == chr_1, fct_1e3 = fct_1e3 == "a")
bench(">", scalar = dbl_1 > 0.5, dbl_1e3 = dbl_1e3 > sorted_1e3, dbl_1e6 = dbl_1e6 > sorted_1e6,
      int_1e6 = int_1e6 > 500L, chr_1e3 = chr_1e3 > chr_1)
bench(">=", scalar = dbl_1 >= 0.5, dbl_1e3 = dbl_1e3 >= sorted_1e3, dbl_1e6 = dbl_1e6 >= sorted_1e6,
      int_1e6 = int_1e6 >= 500L)
bench("@", setup = { cls <- getClass("numeric") }, scalar = cls@className)
bench("@<-", setup = { cls <- getClass("numeric"); package <- "methods" }, scalar = cls@package <- package)
bench("[", setup = { idx <- 1:100; big <- sample.int(1e6L, 1e5L) },
      scalar = dbl_1e3[5L], dbl_1e3 = dbl_1e3[idx], neg_1e3 = dbl_1e3[-idx], lgl_1e3 = dbl_1e3[lgl_1e3],
      dbl_1e6 = dbl_1e6[big], lgl_1e6 = dbl_1e6[lgl_1e6], chr_1e3 = chr_1e3[idx], lst_1e3 = lst_1e3[idx],
      named_1e3 = named_1e3[c("n0000005", "n0000500")], mat_100 = mat_100[idx[1:10], ],
      mat_100_cell = mat_100[5L, 7L])
bench("[.AsIs", setup = { x <- I(dbl_1e3); idx <- 1:100 }, dbl_1e3 = `[.AsIs`(x, idx))
bench("[.data.frame", setup = { idx <- 1:100; big <- sample.int(1e6L, 1e5L) },
      rows_1e3 = `[.data.frame`(df_1e3, idx, ), cols_1e3 = `[.data.frame`(df_1e3, c("x", "y")),
      cell_1e3 = `[.data.frame`(df_1e3, 5L, "x"), lgl_1e3 = `[.data.frame`(df_1e3, lgl_1e3, ),
      small = `[.data.frame`(df_10, idx[1:3], ), rows_1e6 = `[.data.frame`(df_1e6, big, ))
bench("[.Date", setup = { idx <- 1:100 }, date_1e3 = `[.Date`(date_1e3, idx), date_1e6 = `[.Date`(date_1e6, lgl_1e6))
bench("[.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins"); idx <- 1:100 }, dbl_1e3 = `[.difftime`(d, idx))
bench("[.Dlist", setup = { x <- structure(chr_1e3, names = chr_1e3, class = "Dlist"); idx <- 1:100 },
      chr_1e3 = `[.Dlist`(x, idx))
bench("[.DLLInfoList", setup = { dlls <- getLoadedDLLs(); which <- c("base", "stats") }, small = `[.DLLInfoList`(dlls, which))
bench("[.factor", setup = { idx <- 1:100; big <- sample.int(1e6L, 1e5L) }, fct_1e3 = `[.factor`(fct_1e3, idx),
      drop_1e3 = `[.factor`(fct_1e3, idx, drop = TRUE), fct_1e6 = `[.factor`(fct_1e6, big))
bench("[.hexmode", setup = { h <- as.hexmode(int_1e3); idx <- 1:100 }, int_1e3 = `[.hexmode`(h, idx))
bench("[.listof", setup = { x <- structure(lst_1e3, class = "listof"); idx <- 1:100 }, lst_1e3 = `[.listof`(x, idx))
bench("[.noquote", setup = { x <- noquote(chr_1e3); idx <- 1:100 }, chr_1e3 = `[.noquote`(x, idx))
bench("[.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
  idx <- 1:100
}, small = `[.numeric_version`(v, 5L), int_1e3 = `[.numeric_version`(v, idx))
bench("[.octmode", setup = { o <- as.octmode(int_1e3); idx <- 1:100 }, int_1e3 = `[.octmode`(o, idx))
bench("[.POSIXct", setup = { idx <- 1:100 }, time_1e3 = `[.POSIXct`(time_1e3, idx),
      time_1e6 = `[.POSIXct`(time_1e6, lgl_1e6))
bench("[.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3); idx <- 1:100 }, scalar = `[.POSIXlt`(lt, 5L),
      time_1e3 = `[.POSIXlt`(lt, idx))
bench("[.simple.list", setup = { x <- structure(lst_1e3, class = "simple.list"); idx <- 1:100 },
      lst_1e3 = `[.simple.list`(x, idx))
bench("[.table", setup = { tab <- table(fct_1e3); tab2 <- table(df_1e3$g, df_1e3$b); idx <- 1:5 },
      small = tab[idx], two_way = tab2[idx, ])
bench("[.warnings", setup = {
  w <- structure(rep(list(quote(f(x))), 20L), names = sprintf("warning %d", 1:20), class = "warnings")
  idx <- 1:5
}, small = `[.warnings`(w, idx))
bench("[<-", setup = { y <- dbl_1e3; z <- dbl_1e6; l <- lst_1e3; m <- mat_100; idx <- 1:100; value <- 0 },
      scalar = y[5L] <- value, dbl_1e3 = y[idx] <- value, lgl_1e3 = y[lgl_1e3] <- value,
      dbl_1e6 = z[lgl_1e6] <- value, lst_1e3 = l[idx] <- list(value), mat_100 = m[idx[1:10], ] <- value)
bench("[<-.data.frame", setup = { y <- df_1e3; z <- df_1e6; idx <- 1:100; value <- 0 },
      cell_1e3 = y[5L, "x"] <- value, rows_1e3 = y[idx, "x"] <- value, col_1e3 = y[, "x"] <- dbl_1e3,
      lgl_1e6 = z[lgl_1e6, "x"] <- value)
bench("[<-.Date", setup = { y <- date_1e3; idx <- 1:100 }, date_1e3 = y[idx] <- date_1)
bench("[<-.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins"); idx <- 1:100; v <- as.difftime(5, units = "mins") },
      dbl_1e3 = d[idx] <- v)
bench("[<-.factor", setup = { y <- fct_1e3; z <- fct_1e6; idx <- 1:100; value <- "b" },
      fct_1e3 = y[idx] <- value, fct_1e6 = z[lgl_1e6] <- value)
bench("[<-.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
  value <- numeric_version("1.2.3")
}, scalar = v[5L] <- value)
bench("[<-.POSIXct", setup = { y <- time_1e3; z <- time_1e6; idx <- 1:100 }, time_1e3 = y[idx] <- time_1,
      time_1e6 = z[lgl_1e6] <- time_1)
bench("[<-.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3); value <- as.POSIXlt(time_1); idx <- 1:100 },
      scalar = lt[5L] <- value, time_1e3 = lt[idx] <- value)
bench("[[", scalar = lst_1e3[[500L]], named_1e3 = named_1e3[["n0000500"]], lst_10 = lst_10[["f"]][[2L]],
      env_1e3 = env_1e3[["v0500"]], df_1e3 = df_1e3[["x"]], dbl_1e6 = dbl_1e6[[500000L]])
bench("[[.data.frame", setup = { column <- "x" }, col_1e3 = `[[.data.frame`(df_1e3, column),
      cell_1e3 = `[[.data.frame`(df_1e3, 5L, column))
bench("[[.Date", setup = { i <- 5L }, date_1e3 = `[[.Date`(date_1e3, i))
bench("[[.factor", setup = { i <- 5L }, fct_1e3 = `[[.factor`(fct_1e3, i))
bench("[[.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
  i <- 5L
}, scalar = `[[.numeric_version`(v, i), part = `[[.numeric_version`(v, i, 2L))
bench("[[.POSIXct", setup = { i <- 5L }, time_1e3 = `[[.POSIXct`(time_1e3, i))
bench("[[.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3); i <- 5L }, time_1e3 = `[[.POSIXlt`(lt, i))
bench("[[<-", setup = { l <- lst_1e3; y <- dbl_1e3; e <- new.env(); value <- 1 },
      lst_1e3 = l[[500L]] <- value, dbl_1e3 = y[[500L]] <- value, env = e[["a"]] <- value,
      key = l[["key"]] <- value)
bench("[[<-.data.frame", setup = { y <- df_1e3; z <- df_1e6 }, col_1e3 = y[["z"]] <- dbl_1e3,
      cell_1e3 = y[[5L, "x"]] <- dbl_1, col_1e6 = z[["z"]] <- dbl_1e6)
bench("[[<-.factor", setup = { y <- fct_1e3; value <- "b" }, fct_1e3 = y[[5L]] <- value)
bench("[[<-.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
  value <- "1.2.3"
  part <- 4L
}, scalar = v[[5L]] <- value, part = v[[5L, 2L]] <- part)
bench("[[<-.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3); value <- as.POSIXlt(time_1) }, scalar = lt[[5L]] <- value)
bench("^", scalar = dbl_1^2, dbl_1e3 = pos_1e3^1.5, dbl_1e6 = pos_1e6^1.5, square_1e6 = dbl_1e6^2,
      int_1e6 = int_1e6^2L)
skip(".__H__.cbind", "defunct")
skip(".__H__.rbind", "defunct")
bench("abbreviate", scalar = abbreviate(txt_1, 8L), txt_1e3 = abbreviate(txt_1e3, 8L), chr_1e3 = abbreviate(chr_1e3, 5L))
bench("abs", scalar = abs(dbl_1), dbl_1e3 = abs(dbl_1e3), dbl_1e6 = abs(dbl_1e6), int_1e3 = abs(int_1e3),
      int_1e6 = abs(int_1e6), mat_100 = abs(mat_100))
bench("acos", scalar = acos(dbl_1), dbl_1e3 = acos(prob_1e3), dbl_1e6 = acos(prob_1e6))
bench("acosh", setup = { x <- 1 + pos_1e3 }, scalar = acosh(int_1), dbl_1e3 = acosh(x))
bench("activeBindingFunction", setup = { e <- new.env(); makeActiveBinding("a", function() 1, e); name <- "a" },
      scalar = activeBindingFunction(name, e))
bench("addNA", fct_1e3 = addNA(fct_1e3), fct_1e6 = addNA(fct_1e6))
bench("addTaskCallback", setup = { callback <- function(...) TRUE },
      scalar = removeTaskCallback(addTaskCallback(callback)))
bench("agrep", setup = { pattern <- "prise" }, scalar = agrep(pattern, txt_1),
      txt_1e3 = agrep(pattern, txt_1e3, max.distance = 1), value_1e3 = agrep(pattern, txt_1e3, value = TRUE))
bench("agrepl", setup = { pattern <- "prise" }, scalar = agrepl(pattern, txt_1), txt_1e3 = agrepl(pattern, txt_1e3))
bench("alist", scalar = alist(x = , y = dbl_1))
bench("all", setup = { yes_1e3 <- rep(TRUE, 1e3); yes_1e6 <- rep(TRUE, 1e6) }, scalar = all(lgl_1),
      true_1e3 = all(yes_1e3), true_1e6 = all(yes_1e6), lgl_1e6 = all(lgl_1e6), na_rm_1e3 = all(yes_1e3, na.rm = TRUE))
bench("all.equal", setup = { near_1e3 <- dbl_1e3 + 1e-10; near_1e6 <- dbl_1e6 + 1e-10; other <- df_1e3 },
      scalar = all.equal(dbl_1, 0.7341), dbl_1e3 = all.equal(dbl_1e3, near_1e3), dbl_1e6 = all.equal(dbl_1e6, near_1e6),
      chr_1e3 = all.equal(chr_1e3, chr_1e3), lst_10 = all.equal(lst_10, lst_10), df_1e3 = all.equal(df_1e3, other))
bench("all.equal.character", setup = { other <- txt_1e3 }, chr_1e3 = all.equal.character(chr_1e3, chr_1e3),
      differ_1e3 = all.equal.character(chr_1e3, other))
bench("all.equal.default", setup = { e <- quote(x + 1) }, small = all.equal.default(e, e))
bench("all.equal.environment", setup = { e <- new.env(); assign("a", 1, e); f <- new.env(); assign("a", 1, f) },
      small = all.equal.environment(e, f), env_1e3 = all.equal.environment(env_1e3, env_1e3))
bench("all.equal.envRefClass", setup = {
  generator <- setRefClass("BenchAccount", fields = list(balance = "numeric"), where = environment())
  a <- generator$new(balance = 1)
  b <- generator$new(balance = 1)
}, small = all.equal.envRefClass(a, b))
bench("all.equal.factor", setup = { other <- fct_1e3 }, fct_1e3 = all.equal.factor(fct_1e3, other))
bench("all.equal.formula", setup = { other <- y ~ x + g }, small = all.equal.formula(formula_1, other))
bench("all.equal.function", setup = { other <- function(x) x + 1 }, small = all.equal.function(fun, other))
bench("all.equal.language", setup = { a <- quote(f(x, y + 1)); b <- quote(f(x, y + 2)) }, small = all.equal.language(a, b))
bench("all.equal.list", setup = { other <- lst_1e3 }, lst_10 = all.equal.list(lst_10, lst_10),
      lst_1e3 = all.equal.list(lst_1e3, other))
bench("all.equal.numeric", setup = { near_1e3 <- dbl_1e3 + 1e-10; near_1e6 <- dbl_1e6 + 1e-10 },
      scalar = all.equal.numeric(dbl_1, 0.7341), dbl_1e3 = all.equal.numeric(dbl_1e3, near_1e3),
      dbl_1e6 = all.equal.numeric(dbl_1e6, near_1e6))
bench("all.equal.POSIXt", setup = { other <- time_1e3 + 1 }, time_1e3 = all.equal.POSIXt(time_1e3, other))
bench("all.equal.raw", setup = { other <- rev(raw_1e3) }, raw_1e3 = all.equal.raw(raw_1e3, other))
bench("all.names", setup = { e <- body(lm) }, small = all.names(formula_1), body = all.names(e))
bench("all.vars", setup = { e <- body(lm) }, small = all.vars(formula_1), body = all.vars(e))
bench("allowInterrupts", scalar = allowInterrupts(dbl_1))
bench(".amatch_bounds", setup = { bound <- 0.1; bounds <- list(insertions = 1, all = 2) },
      scalar = .amatch_bounds(bound), list = .amatch_bounds(bounds))
bench(".amatch_costs", setup = { costs <- list(insertions = 2) }, small = .amatch_costs(costs))
bench("any", setup = { no_1e3 <- rep(FALSE, 1e3); no_1e6 <- rep(FALSE, 1e6) }, scalar = any(lgl_1),
      false_1e3 = any(no_1e3), false_1e6 = any(no_1e6), lgl_1e6 = any(lgl_1e6), na_rm_1e3 = any(no_1e3, na.rm = TRUE))
bench("anyDuplicated", scalar = anyDuplicated(dbl_1), dbl_1e3 = anyDuplicated(dbl_1e3), int_1e3 = anyDuplicated(int_1e3),
      chr_1e3 = anyDuplicated(chr_1e3), dbl_1e6 = anyDuplicated(dbl_1e6), chr_1e6 = anyDuplicated(chr_1e6),
      df_1e3 = anyDuplicated(df_1e3))
bench("anyDuplicated.array", setup = { a <- array(int_1e3 %% 3L, c(10L, 10L, 10L)) }, small = anyDuplicated.array(a))
bench("anyDuplicated.data.frame", df_10 = anyDuplicated.data.frame(df_10), df_1e3 = anyDuplicated.data.frame(df_1e3))
bench("anyDuplicated.default", dbl_1e3 = anyDuplicated.default(dbl_1e3), dbl_1e6 = anyDuplicated.default(dbl_1e6))
bench("anyDuplicated.matrix", mat_100 = anyDuplicated.matrix(mat_100), imat_100 = anyDuplicated.matrix(imat_100))
bench("anyNA", scalar = anyNA(dbl_1), dbl_1e3 = anyNA(dbl_1e3), dbl_1e6 = anyNA(dbl_1e6), na_1e6 = anyNA(na_1e6),
      chr_1e6 = anyNA(chr_1e6), lst_1e3 = anyNA(lst_1e3, recursive = TRUE))
bench("anyNA.data.frame", df_1e3 = anyNA.data.frame(df_1e3), df_1e6 = anyNA.data.frame(df_1e6))
bench("anyNA.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
}, int_1e3 = anyNA.numeric_version(v))
bench("anyNA.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = anyNA.POSIXlt(lt))
bench("aperm", setup = { a <- array(dbl_1e3, c(10L, 10L, 10L)); perm <- c(3L, 1L, 2L) }, small = aperm(a, perm),
      mat_100 = aperm(mat_100), mat_1000 = aperm(mat_1000))
bench("aperm.default", setup = { a <- array(dbl_1e3, c(10L, 10L, 10L)); perm <- c(3L, 1L, 2L) },
      small = aperm.default(a, perm))
bench("aperm.table", setup = { tab <- table(df_1e3$g, df_1e3$b) }, small = aperm.table(tab))
bench("append", scalar = append(dbl_1, 2), dbl_1e3 = append(dbl_1e3, dbl_1, after = 500L),
      lst_1e3 = append(lst_1e3, lst_10), dbl_1e6 = append(dbl_1e6, dbl_1e3))
bench("apply", setup = { small <- mat_10 }, mat_10 = apply(small, 1L, sum), mat_100 = apply(mat_100, 2L, max),
      mat_1000 = apply(mat_1000, 1L, sum), both_100 = apply(mat_100, c(1L, 2L), abs))
bench("Arg", scalar = Arg(cpl_1), cpl_1e3 = Arg(cpl_1e3), cpl_1e6 = Arg(cpl_1e6), dbl_1e3 = Arg(dbl_1e3))
bench("args", closure = args(fun), builtin = args(sum))
bench("array", setup = { dims <- c(10L, 10L, 10L); big <- c(100L, 100L, 100L) }, small = array(dbl_1e3, dims),
      dbl_1e6 = array(dbl_1e6, big), dimnames = array(int_1e3, c(10L, 100L), list(letters[1:10], NULL)))
bench("array2DF", setup = { a <- tapply(df_1e3$x, list(df_1e3$g, df_1e3$b), sum) }, small = array2DF(a))
bench("arrayInd", setup = { dims <- c(100L, 100L); big <- c(1000L, 1000L) }, scalar = arrayInd(5000L, dims),
      int_1e3 = arrayInd(int_1e3, dims), int_1e6 = arrayInd(int_1e6, big))
bench("as.array", dbl_1e3 = as.array(dbl_1e3), named_1e3 = as.array(named_1e3), mat_100 = as.array(mat_100))
bench("as.array.default", dbl_1e3 = as.array.default(dbl_1e3), named_1e3 = as.array.default(named_1e3))
bench("as.call", setup = { l <- list(as.name("sum"), 1, 2); long <- c(list(as.name("c")), lst_1e3) },
      small = as.call(l), lst_1e3 = as.call(long))
bench("as.character", scalar = as.character(dbl_1), dbl_1e3 = as.character(dbl_1e3), int_1e3 = as.character(int_1e3),
      dbl_1e6 = as.character(dbl_1e6), int_1e6 = as.character(int_1e6), lgl_1e3 = as.character(lgl_1e3),
      fct_1e3 = as.character(fct_1e3), lst_1e3 = as.character(lst_1e3), date_1e3 = as.character(date_1e3),
      chr_1e6 = as.character(chr_1e6))
bench("as.character.condition", setup = { condition <- simpleCondition("something happened", call = quote(f(x))) },
      scalar = as.character.condition(condition))
bench("as.character.Date", scalar = as.character.Date(date_1), date_1e3 = as.character.Date(date_1e3))
bench("as.character.default", dbl_1e3 = as.character.default(dbl_1e3), int_1e6 = as.character.default(int_1e6))
bench("as.character.error", setup = { condition <- simpleError("boom", call = quote(f(x))) },
      scalar = as.character.error(condition))
bench("as.character.factor", fct_1e3 = as.character.factor(fct_1e3), fct_1e6 = as.character.factor(fct_1e6))
bench("as.character.hexmode", setup = { h <- as.hexmode(int_1e3) }, int_1e3 = as.character.hexmode(h))
bench("as.character.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
}, int_1e3 = as.character.numeric_version(v))
bench("as.character.octmode", setup = { o <- as.octmode(int_1e3) }, int_1e3 = as.character.octmode(o))
bench("as.character.POSIXt", scalar = as.character.POSIXt(time_1), time_1e3 = as.character.POSIXt(time_1e3))
bench("as.character.srcref", setup = {
  parsed <- parse(text = c("f <- function(x) {", "  x + 1", "}"), keep.source = TRUE)
  ref <- attr(parsed, "srcref")[[1L]]
}, small = as.character.srcref(ref))
bench("as.complex", scalar = as.complex(dbl_1), dbl_1e3 = as.complex(dbl_1e3), dbl_1e6 = as.complex(dbl_1e6),
      chr_1e3 = as.complex(numstr_1e3))
bench("as.data.frame", setup = { m <- mat_100; l <- lst_10[c(1, 3, 7)] }, dbl_1e3 = as.data.frame(dbl_1e3), lst_10 = as.data.frame(l),
      mat_100 = as.data.frame(m), df_1e3 = as.data.frame(df_1e3), chr_1e3 = as.data.frame(chr_1e3))
bench("as.data.frame.array", setup = { a <- array(dbl_1e3, c(100L, 10L)) }, small = as.data.frame.array(a))
bench("as.data.frame.AsIs", setup = { x <- I(dbl_1e3) }, dbl_1e3 = as.data.frame.AsIs(x))
bench("as.data.frame.character", chr_1e3 = as.data.frame.character(chr_1e3), chr_1e6 = as.data.frame.character(chr_1e6))
bench("as.data.frame.complex", cpl_1e3 = as.data.frame(cpl_1e3))
bench("as.data.frame.data.frame", df_1e3 = as.data.frame.data.frame(df_1e3), df_10 = as.data.frame.data.frame(df_10))
bench("as.data.frame.Date", date_1e3 = as.data.frame(date_1e3))
bench("as.data.frame.default", dbl_1e3 = as.data.frame.default(dbl_1e3))
bench("as.data.frame.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins") }, dbl_1e3 = as.data.frame(d))
bench("as.data.frame.factor", fct_1e3 = as.data.frame(fct_1e3), fct_1e6 = as.data.frame(fct_1e6))
bench("as.data.frame.integer", int_1e3 = as.data.frame(int_1e3), int_1e6 = as.data.frame(int_1e6))
bench("as.data.frame.list", setup = { l <- list(a = dbl_1e3, b = chr_1e3, c = int_1e3); wide <- as.list(df_1e3); small <- lst_10[c(1, 3, 7)] },
      small = as.data.frame.list(small), lst_1e3 = as.data.frame.list(l), df_1e3 = as.data.frame.list(wide))
bench("as.data.frame.logical", lgl_1e3 = as.data.frame(lgl_1e3))
bench("as.data.frame.matrix", mat_100 = as.data.frame.matrix(mat_100), mat_1000 = as.data.frame.matrix(mat_1000))
bench("as.data.frame.model.matrix", setup = { mm <- model.matrix(~ x + g, model_1e3) },
      small = as.data.frame.model.matrix(mm))
bench("as.data.frame.noquote", setup = { x <- noquote(chr_1e3) }, chr_1e3 = as.data.frame(x))
bench("as.data.frame.numeric", dbl_1e3 = as.data.frame(dbl_1e3), dbl_1e6 = as.data.frame(dbl_1e6))
bench("as.data.frame.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
}, int_1e3 = as.data.frame(v))
bench("as.data.frame.ordered", setup = { o <- as.ordered(fct_1e3) }, fct_1e3 = as.data.frame(o))
bench("as.data.frame.POSIXct", time_1e3 = as.data.frame(time_1e3))
bench("as.data.frame.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = as.data.frame.POSIXlt(lt))
bench("as.data.frame.raw", raw_1e3 = as.data.frame(raw_1e3))
bench("as.data.frame.table", setup = { tab <- table(df_1e3$g, df_1e3$b) }, small = as.data.frame.table(tab))
bench("as.data.frame.ts", ts_1e3 = as.data.frame.ts(ts_1e3))
bench("as.data.frame.vector", dbl_1e3 = as.data.frame.vector(dbl_1e3), dbl_1e6 = as.data.frame.vector(dbl_1e6))
bench("as.Date", setup = { f <- factor(datestr_1e3); s <- "2021-06-15" }, scalar = as.Date(s),
      datestr_1e3 = as.Date(datestr_1e3), format_1e3 = as.Date(datestr_1e3, "%Y-%m-%d"),
      int_1e3 = as.Date(int_1e3), time_1e3 = as.Date(time_1e3), fct_1e3 = as.Date(f), date_1e3 = as.Date(date_1e3))
bench("as.Date.character", setup = { s <- "2021-06-15" }, scalar = as.Date.character(s),
      datestr_1e3 = as.Date.character(datestr_1e3),
      format_1e3 = as.Date.character(datestr_1e3, format = "%Y-%m-%d"))
bench("as.Date.default", setup = { x <- as.logical(rep(NA, 1e3)) }, lgl_1e3 = as.Date.default(x))
bench("as.Date.factor", setup = { f <- factor(datestr_1e3) }, fct_1e3 = as.Date.factor(f))
bench("as.Date.numeric", scalar = as.Date.numeric(dbl_1), int_1e3 = as.Date.numeric(int_1e3),
      int_1e6 = as.Date.numeric(int_1e6))
bench("as.Date.POSIXct", scalar = as.Date.POSIXct(time_1), time_1e3 = as.Date.POSIXct(time_1e3),
      time_1e6 = as.Date.POSIXct(time_1e6), tz_1e3 = as.Date.POSIXct(time_1e3, tz = "America/New_York"))
bench("as.Date.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = as.Date.POSIXlt(lt))
bench("as.difftime", setup = { clock <- rep(c("0:30:00", "12:15:30"), 500L) }, scalar = as.difftime(dbl_1, units = "mins"),
      dbl_1e3 = as.difftime(dbl_1e3, units = "hours"), chr_1e3 = as.difftime(clock))
bench("as.double", scalar = as.double(int_1), int_1e3 = as.double(int_1e3), int_1e6 = as.double(int_1e6),
      chr_1e3 = as.double(numstr_1e3), chr_1e6 = as.double(numstr_1e6), lgl_1e3 = as.double(lgl_1e3))
bench("as.double.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins") }, dbl_1e3 = as.double.difftime(d),
      units_1e3 = as.double.difftime(d, units = "hours"))
bench("as.double.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = as.double.POSIXlt(lt))
bench("as.environment", setup = { n <- 1L; l <- list(a = 1, b = 2); name <- "package:base" }, number = as.environment(n),
      env = as.environment(env_1e3), lst_10 = as.environment(l), search = as.environment(name))
bench("as.expression", setup = { e <- quote(x + 1) }, small = as.expression(e), dbl_1e3 = as.expression(dbl_1e3))
bench("as.expression.default", setup = { e <- quote(x + 1) }, small = as.expression.default(e))
bench("as.factor", scalar = as.factor(chr_1), chr_1e3 = as.factor(chr_1e3), int_1e3 = as.factor(int_1e3),
      int_1e6 = as.factor(int_1e6), fct_1e3 = as.factor(fct_1e3))
bench("as.function", setup = { a <- alist(x = , x + 1) }, small = as.function(a))
bench("as.function.default", setup = { a <- alist(x = , y = 2, x + y) }, small = as.function.default(a))
bench("as.hexmode", setup = { hex <- c("ff", "1a", "7") }, scalar = as.hexmode(int_1), int_1e3 = as.hexmode(int_1e3),
      chr = as.hexmode(hex))
bench("as.integer", setup = { int_str <- as.character(int_1e3) }, scalar = as.integer(dbl_1), dbl_1e3 = as.integer(dbl_1e3), dbl_1e6 = as.integer(dbl_1e6),
      chr_1e3 = as.integer(int_str), lgl_1e6 = as.integer(lgl_1e6),
      fct_1e3 = as.integer(fct_1e3), int_1e6 = as.integer(int_1e6))
bench("as.list", dbl_1e3 = as.list(dbl_1e3), dbl_1e6 = as.list(dbl_1e6), df_1e3 = as.list(df_1e3),
      env_1e3 = as.list(env_1e3), lst_10 = as.list(lst_10), fct_1e3 = as.list(fct_1e3))
bench("as.list.data.frame", df_1e3 = as.list.data.frame(df_1e3), df_10 = as.list.data.frame(df_10))
bench("as.list.Date", date_1e3 = as.list.Date(date_1e3))
bench("as.list.default", dbl_1e3 = as.list.default(dbl_1e3), dbl_1e6 = as.list.default(dbl_1e6))
bench("as.list.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins") }, dbl_1e3 = as.list.difftime(d))
bench("as.list.environment", env_1e3 = as.list.environment(env_1e3), sorted = as.list.environment(env_1e3, sorted = TRUE))
bench("as.list.factor", fct_1e3 = as.list.factor(fct_1e3))
bench("as.list.function", closure = as.list.function(fun), lm = as.list.function(lm))
bench("as.list.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
}, int_1e3 = as.list.numeric_version(v))
bench("as.list.POSIXct", time_1e3 = as.list.POSIXct(time_1e3))
bench("as.list.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = as.list.POSIXlt(lt))
bench("as.logical", setup = { words <- rep(c("TRUE", "false", "T", "no"), 250L) }, scalar = as.logical(int_1), int_1e3 = as.logical(int_1e3), int_1e6 = as.logical(int_1e6),
      chr_1e3 = as.logical(words), dbl_1e6 = as.logical(dbl_1e6))
bench("as.logical.factor", setup = { f <- factor(rep(c("TRUE", "FALSE"), 500L)) }, fct_1e3 = as.logical.factor(f))
bench("as.matrix", setup = { num_1e3 <- df_1e3[c("x", "y")]; num_1e6 <- df_1e6[c("x", "y")] }, dbl_1e3 = as.matrix(dbl_1e3),
      df_1e3 = as.matrix(num_1e3), mixed_1e3 = as.matrix(df_1e3), mat_100 = as.matrix(mat_100), df_1e6 = as.matrix(num_1e6))
bench("as.matrix.data.frame", setup = { num <- df_1e3[c("id", "x", "y")] }, num_1e3 = as.matrix.data.frame(num),
      mixed_1e3 = as.matrix.data.frame(df_1e3), df_10 = as.matrix.data.frame(df_10))
bench("as.matrix.default", dbl_1e3 = as.matrix.default(dbl_1e3), dbl_1e6 = as.matrix.default(dbl_1e6))
bench("as.matrix.noquote", setup = { x <- noquote(chr_1e3) }, chr_1e3 = as.matrix.noquote(x))
bench("as.matrix.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = as.matrix.POSIXlt(lt))
bench("as.name", scalar = as.name(chr_1))
bench("as.null", scalar = as.null(dbl_1), lst_10 = as.null(lst_10))
bench("as.null.default", scalar = as.null.default(dbl_1))
bench("as.numeric", scalar = as.numeric(int_1), int_1e3 = as.numeric(int_1e3), int_1e6 = as.numeric(int_1e6),
      chr_1e3 = as.numeric(numstr_1e3), chr_1e6 = as.numeric(numstr_1e6), lgl_1e3 = as.numeric(lgl_1e3),
      dbl_1e6 = as.numeric(dbl_1e6), fct_1e3 = as.numeric(fct_1e3))
bench("as.numeric_version", setup = { versions <- sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3); v <- "4.6.2" },
      scalar = as.numeric_version(v), chr_1e3 = as.numeric_version(versions))
bench("as.octmode", scalar = as.octmode(int_1), int_1e3 = as.octmode(int_1e3))
bench("as.ordered", chr_1e3 = as.ordered(chr_1e3), fct_1e3 = as.ordered(fct_1e3), int_1e6 = as.ordered(int_1e6))
bench("as.package_version", setup = { versions <- sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3); v <- "4.6.2" },
      scalar = as.package_version(v), chr_1e3 = as.package_version(versions))
bench("as.pairlist", lst_10 = as.pairlist(lst_10), lst_1e3 = as.pairlist(lst_1e3))
bench("as.POSIXct", setup = { stamps <- format(time_1e3, "%Y-%m-%d %H:%M:%S"); seconds <- dbl_1e3 * 1e6; lt <- as.POSIXlt(time_1e3) },
      scalar = as.POSIXct(date_1), date_1e3 = as.POSIXct(date_1e3), chr_1e3 = as.POSIXct(stamps, tz = "UTC"),
      dbl_1e3 = as.POSIXct(seconds, tz = "UTC"), lt_1e3 = as.POSIXct(lt), date_1e6 = as.POSIXct(date_1e6))
bench("as.POSIXct.Date", scalar = as.POSIXct.Date(date_1), date_1e3 = as.POSIXct.Date(date_1e3),
      date_1e6 = as.POSIXct.Date(date_1e6))
bench("as.POSIXct.default", setup = { stamps <- format(time_1e3, "%Y-%m-%d %H:%M:%S") },
      chr_1e3 = as.POSIXct.default(stamps, tz = "UTC"), time_1e3 = as.POSIXct.default(time_1e3))
bench("as.POSIXct.numeric", scalar = as.POSIXct.numeric(dbl_1, tz = "UTC"), dbl_1e3 = as.POSIXct.numeric(dbl_1e3, tz = "UTC"),
      dbl_1e6 = as.POSIXct.numeric(dbl_1e6, tz = "UTC"))
bench("as.POSIXct.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3); local <- as.POSIXlt(time_1e3, tz = "America/New_York") },
      time_1e3 = as.POSIXct.POSIXlt(lt), tz_1e3 = as.POSIXct.POSIXlt(local))
bench("as.POSIXlt", setup = { stamps <- format(time_1e3, "%Y-%m-%d %H:%M:%S") }, scalar = as.POSIXlt(time_1),
      time_1e3 = as.POSIXlt(time_1e3), time_1e6 = as.POSIXlt(time_1e6), chr_1e3 = as.POSIXlt(stamps, tz = "UTC"),
      date_1e3 = as.POSIXlt(date_1e3), tz_1e3 = as.POSIXlt(time_1e3, tz = "Europe/Berlin"))
bench("as.POSIXlt.character", setup = { stamps <- format(time_1e3, "%Y-%m-%d %H:%M:%S"); s <- "2021-06-15 12:34:56" },
      scalar = as.POSIXlt.character(s, tz = "UTC"),
      chr_1e3 = as.POSIXlt.character(stamps, tz = "UTC"), datestr_1e3 = as.POSIXlt.character(datestr_1e3, tz = "UTC"),
      format_1e3 = as.POSIXlt.character(stamps, tz = "UTC", format = "%Y-%m-%d %H:%M:%S"))
bench("as.POSIXlt.Date", date_1e3 = as.POSIXlt.Date(date_1e3), date_1e6 = as.POSIXlt.Date(date_1e6))
bench("as.POSIXlt.default", setup = { x <- as.logical(rep(NA, 1e3)) }, lgl_1e3 = as.POSIXlt.default(x))
bench("as.POSIXlt.factor", setup = { f <- factor(datestr_1e3) }, fct_1e3 = as.POSIXlt.factor(f, tz = "UTC"))
bench("as.POSIXlt.numeric", setup = { seconds <- dbl_1e3 * 1e6 }, dbl_1e3 = as.POSIXlt.numeric(seconds, tz = "UTC"))
bench("as.POSIXlt.POSIXct", scalar = as.POSIXlt.POSIXct(time_1), time_1e3 = as.POSIXlt.POSIXct(time_1e3),
      time_1e6 = as.POSIXlt.POSIXct(time_1e6), tz_1e3 = as.POSIXlt.POSIXct(time_1e3, tz = "America/New_York"))
skip("as.qr", "defunct")
bench("as.raw", setup = { bytes_1e3 <- int_1e3 %% 256L; bytes_1e6 <- int_1e6 %% 256L }, scalar = as.raw(int_1),
      int_1e3 = as.raw(bytes_1e3), int_1e6 = as.raw(bytes_1e6))
bench("as.single", dbl_1e3 = as.single(dbl_1e3), int_1e3 = as.single(int_1e3))
bench("as.single.default", dbl_1e3 = as.single.default(dbl_1e3))
bench("as.symbol", scalar = as.symbol(chr_1))
bench("as.table", setup = { m <- matrix(int_1e3[1:100], 10L, 10L) }, small = as.table(m), imat_100 = as.table(imat_100),
      int_1e3 = as.table(int_1e3))
bench("as.table.default", setup = { m <- matrix(int_1e3[1:100], 10L, 10L) }, small = as.table.default(m),
      imat_100 = as.table.default(imat_100))
bench("as.vector", scalar = as.vector(dbl_1), named_1e3 = as.vector(named_1e3), mat_100 = as.vector(mat_100),
      list_1e3 = as.vector(dbl_1e3, "list"), chr_1e3 = as.vector(int_1e3, "character"), named_1e6 = as.vector(named_1e6))
bench("as.vector.data.frame", df_1e3 = as.vector.data.frame(df_1e3), df_10 = as.vector.data.frame(df_10, "list"))
bench("as.vector.factor", fct_1e3 = as.vector.factor(fct_1e3), fct_1e6 = as.vector.factor(fct_1e6))
bench("as.vector.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = as.vector.POSIXlt(lt))
bench("asin", scalar = asin(dbl_1), dbl_1e3 = asin(prob_1e3), dbl_1e6 = asin(prob_1e6))
bench("asinh", scalar = asinh(dbl_1), dbl_1e3 = asinh(dbl_1e3), dbl_1e6 = asinh(dbl_1e6))
bench("asNamespace", setup = { name <- "stats"; ns <- asNamespace("utils") }, chr = asNamespace(name), env = asNamespace(ns))
bench("asplit", mat_100 = asplit(mat_100, 1L), mat_1000 = asplit(mat_1000, 2L))
bench("asS3", setup = { x <- asS4(structure(list(a = 1), class = "thing")) }, scalar = asS3(x))
bench("asS4", setup = { x <- structure(list(a = 1), class = "thing") }, scalar = asS4(x), dbl_1e3 = asS4(dbl_1e3))
bench("assign", setup = { e <- new.env(); name <- "a" }, scalar = assign(name, dbl_1, envir = e),
      env_1e3 = assign("v0500", dbl_1, envir = env_1e3), local = assign(name, dbl_1))
bench("atan", scalar = atan(dbl_1), dbl_1e3 = atan(dbl_1e3), dbl_1e6 = atan(dbl_1e6))
bench("atan2", scalar = atan2(dbl_1, 0.5), dbl_1e3 = atan2(dbl_1e3, pos_1e3), dbl_1e6 = atan2(dbl_1e6, pos_1e6))
bench("atanh", scalar = atanh(dbl_1), dbl_1e3 = atanh(prob_1e3), dbl_1e6 = atanh(prob_1e6))
skip("attach", "changes the session")
skip("attachNamespace", "changes the session")
bench("attr", setup = { which <- "names" }, scalar = attr(named_1e3, which), df_1e3 = attr(df_1e3, "row.names"),
      class = attr(fct_1e3, "class"), levels = attr(fct_1e3, "levels"), partial = attr(fct_1e3, "lev"))
bench("attr.all.equal", setup = { other <- df_1e3 }, df_1e3 = attr.all.equal(df_1e3, other),
      named_1e3 = attr.all.equal(named_1e3, dbl_1e3))
bench("attr<-", setup = { y <- dbl_1e3; z <- dbl_1e6; value <- "x" }, scalar = attr(y, "unit") <- value,
      names_1e3 = attr(y, "names") <- chr_1e3, dim_1e6 = attr(z, "dim") <- c(1000L, 1000L))
bench("attributes", scalar = attributes(dbl_1), named_1e3 = attributes(named_1e3), df_1e3 = attributes(df_1e3),
      fct_1e3 = attributes(fct_1e3))
bench("attributes<-", setup = { y <- dbl_1e3; a <- list(dim = c(10L, 100L), units = "x"); none <- NULL },
      small = attributes(y) <- a, clear = attributes(y) <- none)
skip("autoload", "changes the session")
skip("autoloader", "changes the session")
bench("backsolve", setup = { r <- chol(spd_100); v <- dbl_1e3[1:100]; rhs <- mat_100 }, vec_100 = backsolve(r, v),
      mat_100 = backsolve(r, rhs))
bench("balancePOSIXlt", setup = { lt <- as.POSIXlt(time_1e3); lt$min <- lt$min + 90L }, time_1e3 = balancePOSIXlt(lt))
bench("baseenv", scalar = baseenv())
bench("basename", setup = { paths_1e3 <- file.path("/data/archive", chr_1e3, "file.csv"); path <- "/data/archive/file.csv" },
      scalar = basename(path), chr_1e3 = basename(paths_1e3))
bench("besselI", scalar = besselI(dbl_1, 1), dbl_1e3 = besselI(pos_1e3, 1), expon_1e3 = besselI(pos_1e3, 2.5, TRUE))
bench("besselJ", scalar = besselJ(dbl_1, 1), dbl_1e3 = besselJ(pos_1e3, 1))
bench("besselK", scalar = besselK(dbl_1, 1), dbl_1e3 = besselK(pos_1e3, 1), expon_1e3 = besselK(pos_1e3, 2.5, TRUE))
bench("besselY", scalar = besselY(dbl_1, 1), dbl_1e3 = besselY(pos_1e3, 1))
bench("beta", scalar = beta(dbl_1, 2), dbl_1e3 = beta(prob_1e3, pos_1e3), dbl_1e6 = beta(prob_1e6, 2))
bench(".bincode", setup = { breaks <- seq(-100, 100, by = 10) }, dbl_1e3 = .bincode(dbl_1e3, breaks),
      dbl_1e6 = .bincode(dbl_1e6, breaks))
bench("bindingIsActive", setup = { e <- new.env(); makeActiveBinding("a", function() 1, e); name <- "a" },
      active = bindingIsActive(name, e), env_1e3 = bindingIsActive("v0500", env_1e3))
bench("bindingIsLocked", setup = { e <- new.env(); assign("a", 1, e); lockBinding("a", e); name <- "a" },
      locked = bindingIsLocked(name, e), env_1e3 = bindingIsLocked("v0500", env_1e3))
bench("bindtextdomain", setup = { domain <- "R-base" }, query = bindtextdomain(domain))
bench("bitwAnd", scalar = bitwAnd(int_1, 3L), int_1e3 = bitwAnd(int_1e3, 255L), int_1e6 = bitwAnd(int_1e6, int_1e6))
bench("bitwNot", scalar = bitwNot(int_1), int_1e3 = bitwNot(int_1e3), int_1e6 = bitwNot(int_1e6))
bench("bitwOr", scalar = bitwOr(int_1, 8L), int_1e3 = bitwOr(int_1e3, 255L), int_1e6 = bitwOr(int_1e6, int_1e6))
bench("bitwShiftL", scalar = bitwShiftL(int_1, 2L), int_1e3 = bitwShiftL(int_1e3, 2L), int_1e6 = bitwShiftL(int_1e6, 2L))
bench("bitwShiftR", scalar = bitwShiftR(int_1, 1L), int_1e3 = bitwShiftR(int_1e3, 2L), int_1e6 = bitwShiftR(int_1e6, 2L))
bench("bitwXor", scalar = bitwXor(int_1, 3L), int_1e3 = bitwXor(int_1e3, 255L), int_1e6 = bitwXor(int_1e6, int_1e6))
bench("body", closure = body(fun), lm = body(lm))
bench("body<-", setup = { f <- fun; b <- quote(x * 2) }, closure = body(f) <- b)
bench("bquote", setup = { x <- 5; parts <- list(quote(a), quote(b)) }, small = bquote(.(x) + y),
      nested = bquote(f(.(x), g(.(x), h)), splice = TRUE), splice = bquote(f(..(parts)), splice = TRUE))
skip("break", "language construct")
skip("browser", "interactive")
skip("browserCondition", "interactive")
skip("browserSetDebug", "interactive")
skip("browserText", "interactive")
bench("builtins", setup = { internal <- FALSE }, all = builtins(internal))
bench("by", setup = { g <- df_1e3$g; num <- df_1e3[c("x", "y")] }, df_1e3 = by(num, g, nrow), vec_1e3 = by(df_1e3$x, g, sum))
bench("by.data.frame", setup = { g <- df_1e3$g }, df_1e3 = by.data.frame(df_1e3, g, nrow))
bench("by.default", setup = { g <- df_1e3$g }, dbl_1e3 = by.default(df_1e3$x, g, sum))
skip("bzfile", "reads files")
bench(".C", setup = {
  routine <- stats:::C_kmeans_Lloyd
  x <- dbl_1e3
  centers <- x[c(1L, 2L, 3L, 501L, 502L, 503L)]
}, kmeans = .C(routine, x, 500L, 2L, centers = centers, 3L, c1 = integer(500L), iter = 10L, nc = integer(3L),
               wss = double(3L)))
bench("c", setup = { other <- lst_10 },
      scalar = c(dbl_1, 2, 3), dbl_1e3 = c(dbl_1e3, dbl_1e3), dbl_1e6 = c(dbl_1e6, dbl_1e6), int_1e3 = c(int_1e3, int_1),
      mixed_1e3 = c(int_1e3, dbl_1e3), chr_1e3 = c(chr_1e3, chr_1), chr_1e6 = c(chr_1e6, chr_1e3),
      lst_1e3 = c(lst_1e3, lst_1e3), lst_10 = c(lst_10, other), named_1e3 = c(named_1e3, named_1e3),
      named_scalar = c(a = dbl_1, b = 2), lst_dbl_1e3 = c(lst_1e3, dbl_1e3), recursive_1e3 = c(lst_1e3, recursive = TRUE))
bench("c.Date", scalar = c.Date(date_1, date_1), date_1e3 = c.Date(date_1e3, date_1e3), date_1e6 = c.Date(date_1e6, date_1))
bench("c.difftime", setup = { d <- as.difftime(dbl_1e3, units = "mins"); h <- as.difftime(dbl_1e3, units = "hours") },
      dbl_1e3 = c.difftime(d, d), mixed_1e3 = c.difftime(d, h))
bench("c.factor", setup = { other <- factor(letters[1:3]) }, fct_1e3 = c.factor(fct_1e3, other), fct_1e6 = c.factor(fct_1e6, fct_1e3))
bench("c.noquote", setup = { x <- noquote(chr_1e3) }, chr_1e3 = c.noquote(x, x))
bench("c.numeric_version", setup = {
  v <- numeric_version(sprintf("%d.%d.%d", int_1e3 %% 10L, int_1e3 %% 7L, int_1e3))
}, int_1e3 = c.numeric_version(v, v))
bench("c.POSIXct", scalar = c.POSIXct(time_1, time_1), time_1e3 = c.POSIXct(time_1e3, time_1e3),
      time_1e6 = c.POSIXct(time_1e6, time_1))
bench("c.POSIXlt", setup = { lt <- as.POSIXlt(time_1e3) }, time_1e3 = c.POSIXlt(lt, lt))
bench("c.warnings", setup = {
  w <- structure(rep(list(quote(f(x))), 20L), names = sprintf("warning %d", 1:20), class = "warnings")
}, small = c.warnings(w, w))
bench(".cache_class", setup = { name <- "numeric"; extends <- methods::extends("numeric") }, scalar = .cache_class(name, extends))
bench(".Call", setup = {
  routine <- stats:::C_binomial_dev_resids
  y <- as.double(lgl_1e3)
  y6 <- as.double(lgl_1e6)
  weights <- rep(1, 1e3)
  weights6 <- rep(1, 1e6)
}, dbl_1e3 = .Call(routine, y, prob_1e3, weights), dbl_1e6 = .Call(routine, y6, prob_1e6, weights6))
bench("call", setup = { name <- "sum" }, scalar = call(name, dbl_1), args = call(name, dbl_1, int_1, lst_10))
skip(".Call.graphics", "graphics devices")
bench("callCC", scalar = callCC(function(k) k(dbl_1)))
bench("capabilities", setup = { what <- c("fifo", "sockets", "long.double") }, small = capabilities(what))
bench("casefold", scalar = casefold(txt_1, upper = TRUE), chr_1e3 = casefold(chr_1e3, upper = TRUE),
      txt_1e3 = casefold(txt_1e3))
bench("cat", setup = { lines <- txt_1e3 }, scalar = cat(dbl_1), dbl_1e3 = cat(dbl_1e3), txt_1e3 = cat(lines, sep = "\n"),
      mixed = cat("x =", dbl_1, "and", chr_1, "\n"), fill = cat(txt_1e3, fill = 80))
bench("cbind", setup = { small <- dbl_1e3[1:10] }, scalar = cbind(dbl_1, 2), small = cbind(small, small),
      dbl_1e3 = cbind(dbl_1e3, pos_1e3, int_1e3), mat_100 = cbind(mat_100, mat_100), dbl_1e6 = cbind(dbl_1e6, pos_1e6),
      df_1e3 = cbind(df_1e3, z = dbl_1e3), mat_1000 = cbind(mat_1000, dbl_1e3))
bench("cbind.data.frame", setup = { other <- df_1e3[c("x", "y")] }, df_1e3 = cbind.data.frame(df_1e3, other),
      vec_1e3 = cbind.data.frame(df_1e3, z = dbl_1e3), df_10 = cbind.data.frame(df_10, df_10))
bench("ceiling", scalar = ceiling(dbl_1), dbl_1e3 = ceiling(dbl_1e3), dbl_1e6 = ceiling(dbl_1e6), int_1e3 = ceiling(int_1e3))
bench("char.expand", setup = { target <- c("mean", "median", "mode", "max", "min"); input <- "mea" }, scalar = char.expand(input, target))
bench("character", setup = { n_1e3 <- 1000L; n_1e6 <- 1000000L; small <- 3L }, small = character(small),
      int_1e3 = character(n_1e3), int_1e6 = character(n_1e6))
bench("charmatch", setup = {
  table <- c("mean", "median", "mode", "max", "min", "sum")
  input <- "med"
  ids <- chr_1e3[1:100]
  prefixes <- substr(txt_1e3, 1L, 3L)
  heads <- c("alpha", "beta", "gamma", "the", "price")
}, scalar = charmatch(input, table), chr_1e3 = charmatch(chr_1e3, ids), txt_1e3 = charmatch(prefixes, heads))
bench("charToRaw", setup = { long <- paste(txt_1e3, collapse = " ") }, scalar = charToRaw(txt_1), long = charToRaw(long))
bench("chartr", scalar = chartr("abc", "xyz", txt_1), chr_1e3 = chartr("abc", "xyz", chr_1e3),
      txt_1e3 = chartr("aeiou", "AEIOU", txt_1e3))
bench(".check_tzones", setup = { local <- as.POSIXct(time_1e3, tz = "UTC") }, time_1e3 = .check_tzones(time_1e3, local),
      scalar = .check_tzones(time_1))
bench("chkDots", setup = { which <- -1L }, empty = chkDots(which.call = which))
bench("chol", setup = { small <- crossprod(mat_10) + diag(10L) }, mat_10 = chol(small), spd_100 = chol(spd_100))
bench("chol.default", spd_100 = chol.default(spd_100))
bench("chol2inv", setup = { r <- chol(spd_100) }, spd_100 = chol2inv(r))
bench("choose", scalar = choose(int_1e3[5L], 3), int_1e3 = choose(int_1e3, 3L), dbl_1e3 = choose(pos_1e3, 2),
      int_1e6 = choose(int_1e6, 2L))
bench("chooseOpsMethod", setup = { none <- NULL }, scalar = chooseOpsMethod(dbl_1, dbl_1, none, none, none, FALSE))
bench("chooseOpsMethod.default", setup = { none <- NULL }, scalar = chooseOpsMethod.default(dbl_1, dbl_1, none, none, none, FALSE))
bench("class", scalar = class(dbl_1), df_1e3 = class(df_1e3), fct_1e3 = class(fct_1e3), mat_100 = class(mat_100),
      time_1 = class(time_1), fun = class(fun))
bench(".class2", scalar = .class2(dbl_1), mat_100 = .class2(mat_100), df_1e3 = .class2(df_1e3))
bench("class<-", setup = { y <- lst_10; z <- dbl_1e3; value <- "thing"; none <- NULL },
      lst_10 = class(y) <- value, dbl_1e3 = class(z) <- c("a", "b"), clear = class(y) <- none)
bench("clearPushBack", setup = { connection <- textConnection(txt_1e3); pushBack("first line", connection) },
      scalar = clearPushBack(connection))
bench("close", small = close(textConnection(txt_1e3)), raw = close(rawConnection(raw_1e3)))
bench("close.connection", small = close.connection(textConnection(txt_1e3)))
bench("close.srcfile", setup = { file <- srcfilecopy("<text>", txt_1e3) }, small = close.srcfile(file))
bench("close.srcfilealias", setup = { file <- srcfilealias("alias", srcfilecopy("<text>", txt_1e3)) },
      small = close.srcfilealias(file))
skip("closeAllConnections", "changes the session")
bench(".col", setup = { dims <- c(100L, 100L); big <- c(1000L, 1000L) }, mat_100 = .col(dims), mat_1000 = .col(big))
