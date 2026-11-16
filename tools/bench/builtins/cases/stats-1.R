bench("acf",
      ts_1e3 = acf(ts_1e3, plot = FALSE),
      dbl_1e3 = acf(dbl_1e3, lag.max = 20L, plot = FALSE),
      dbl_1e6 = acf(dbl_1e6, lag.max = 20L, plot = FALSE))
bench("acf2AR", setup = { r <- drop(acf(ts_1e3, lag.max = 20L, plot = FALSE)$acf) },
      small = acf2AR(r))
bench("add.scope", setup = { lower <- y ~ x; upper <- y ~ x + g + x:g },
      small = add.scope(lower, upper))
bench("add1", setup = { fit <- lm(y ~ x, model_1e3) },
      lm_1e3 = add1(fit, ~ . + g))
bench("addmargins", setup = { tab <- table(df_1e3$g, df_1e3$b) },
      small = addmargins(tab))
bench("aggregate", setup = { by <- list(g = df_1e3$g); d5 <- df_1e6[1:1e5, ] },
      formula_1e3 = aggregate(x ~ g, df_1e3, mean),
      df_1e3 = aggregate(df_1e3[c("x", "y")], by, mean),
      formula_1e5 = aggregate(x ~ g, d5, mean))
bench("aggregate.data.frame", setup = { by <- list(g = df_1e3$g, b = df_1e3$b) },
      df_1e3 = aggregate.data.frame(df_1e3[c("x", "y")], by, sum))
bench("aggregate.ts",
      ts_1e3 = aggregate.ts(ts_1e3, nfrequency = 4, FUN = mean))
bench("AIC", setup = { fit <- lm(y ~ x + g, model_1e3) },
      lm_1e3 = AIC(fit))
bench("alias", setup = { fit <- lm(y ~ x + g, model_1e3) },
      lm_1e3 = alias(fit))
bench("anova", setup = { fit0 <- lm(y ~ x, model_1e3); fit <- lm(y ~ x + g, model_1e3) },
      lm_1e3 = anova(fit),
      two_lm_1e3 = anova(fit0, fit))
bench("ansari.test", setup = { x <- dbl_1e3[1:20]; y <- pos_1e3[1:25]; u <- dbl_1e3[1:500]; v <- pos_1e3[501:1000] },
      small = ansari.test(x, y),
      dbl_1e3 = ansari.test(u, v))
bench("aov", aov_1e3 = aov(y ~ x + g, model_1e3))
bench("approx", setup = { xout <- seq(-99, 99, length.out = 1000L); x5 <- unique(sorted_1e6[1:1e5]); y5 <- dbl_1e6[seq_along(x5)]; xout5 <- dbl_1e6[1:1e5] },
      dbl_1e3 = approx(sorted_1e3, dbl_1e3, xout = xout),
      dbl_1e5 = approx(x5, y5, xout = xout5))
bench("approxfun", setup = { x6 <- unique(sorted_1e6); y6 <- dbl_1e6[seq_along(x6)] },
      dbl_1e3 = approxfun(sorted_1e3, dbl_1e3),
      dbl_1e6 = approxfun(x6, y6))
bench("ar", ts_1e3 = ar(ts_1e3))
bench("ar.burg", ts_1e3 = ar.burg(ts_1e3))
bench("ar.mle", ts_1e3 = ar.mle(ts_1e3, order.max = 3L))
bench("ar.ols", ts_1e3 = ar.ols(ts_1e3))
bench("ar.yw", ts_1e3 = ar.yw(ts_1e3))
bench("arima", ts_1e3 = arima(ts_1e3, order = c(1L, 1L, 1L)))
bench("arima.sim", setup = { model <- list(ar = 0.5, ma = -0.3) },
      n_1e3 = arima.sim(model, n = 1000L))
bench("arima0", ts_1e3 = arima0(ts_1e3, order = c(1L, 1L, 1L)))
skip("arima0.diag", "defunct")
bench("ARMAacf", setup = { phi <- c(0.5, -0.2); theta <- 0.3 },
      small = ARMAacf(phi, theta, lag.max = 50L))
bench("ARMAtoMA", setup = { phi <- c(0.5, -0.2); theta <- 0.3 },
      small = ARMAtoMA(phi, theta, lag.max = 100L))
bench("as.dendrogram", setup = { hc <- hclust(dist(mat_100)) },
      hclust_100 = as.dendrogram(hc))
bench("as.dist", setup = { m <- as.matrix(dist(mat_100)) },
      mat_100 = as.dist(m))
bench("as.formula", setup = { text <- "y ~ x + g" },
      chr = as.formula(text),
      formula = as.formula(formula_1))
bench("as.hclust", setup = { dend <- as.dendrogram(hclust(dist(mat_100))) },
      dendrogram_100 = as.hclust(dend))
bench("as.stepfun", setup = { fit <- isoreg(dbl_1e3) },
      isoreg_1e3 = as.stepfun(fit))
bench("as.ts", dbl_1e3 = as.ts(dbl_1e3), dbl_1e6 = as.ts(dbl_1e6))
bench("asOneSidedFormula", setup = { name <- "x" }, scalar = asOneSidedFormula(name))
bench("ave",
      dbl_1e3 = ave(df_1e3$x, df_1e3$g),
      dbl_1e6 = ave(df_1e6$x, df_1e6$g))
bench("bandwidth.kernel", setup = { k <- kernel("daniell", 5L) }, small = bandwidth.kernel(k))
bench("bartlett.test", df_1e3 = bartlett.test(x ~ g, df_1e3))
bench("BIC", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = BIC(fit))
bench("binom.test", setup = { k <- 37L }, scalar = binom.test(k, 100L))
bench("binomial", setup = { link <- "logit" }, scalar = binomial(link = link))
skip("biplot", "graphics device")
bench("Box.test",
      ts_1e3 = Box.test(ts_1e3, lag = 10L, type = "Ljung-Box"),
      dbl_1e6 = Box.test(dbl_1e6, lag = 10L))
bench("bw.bcv", setup = { x <- c(rnorm(500L), rnorm(500L, 4)) }, dbl_1e3 = bw.bcv(x))
bench("bw.nrd", dbl_1e3 = bw.nrd(dbl_1e3), dbl_1e6 = bw.nrd(dbl_1e6))
bench("bw.nrd0", dbl_1e3 = bw.nrd0(dbl_1e3), dbl_1e6 = bw.nrd0(dbl_1e6))
bench("bw.SJ", dbl_1e3 = bw.SJ(dbl_1e3))
bench("bw.ucv", setup = { x <- rnorm(1000L) }, dbl_1e3 = bw.ucv(x))
bench("C", setup = { f <- df_1e3$g }, fct_1e3 = C(f, sum))
bench("cancor", setup = { x <- mat_100[, 1:5]; y <- mat_100[, 6:10] }, mat_100 = cancor(x, y))
bench("case.names", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = case.names(fit))
bench("ccf", setup = { y <- pos_1e3 }, dbl_1e3 = ccf(dbl_1e3, y, plot = FALSE))
bench(".checkMFClasses", setup = { fit <- lm(y ~ x + g, model_1e3); classes <- attr(terms(fit), "dataClasses"); mf <- model.frame(fit) },
      lm_1e3 = .checkMFClasses(classes, mf))
bench("chisq.test", setup = { tab <- table(df_1e3$g, df_1e3$b) },
      table = chisq.test(tab),
      dbl_1e3 = chisq.test(pos_1e3))
bench("cmdscale", setup = { d <- dist(mat_100) }, dist_100 = cmdscale(d))
bench("coef", setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      lm_1e3 = coef(fit),
      glm_1e3 = coef(gfit))
bench("coefficients", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = coefficients(fit))
bench("complete.cases",
      na_1e3 = complete.cases(na_1e3),
      df_1e3 = complete.cases(df_1e3),
      na_1e6 = complete.cases(na_1e6))
bench("confint", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = confint(fit))
bench("confint.default", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = confint.default(fit))
bench("confint.lm", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = confint.lm(fit))
bench("constrOptim", setup = {
        f <- function(p) sum((p - c(1, 2))^2)
        ui <- diag(2L); ci <- c(0, 0); start <- c(0.5, 0.5)
      },
      small = constrOptim(start, f, NULL, ui, ci))
bench("contr.helmert", setup = { n <- 10L; n100 <- 100L }, small = contr.helmert(n), n_100 = contr.helmert(n100))
bench("contr.poly", setup = { n <- 10L; n20 <- 20L }, small = contr.poly(n), n_20 = contr.poly(n20))
bench("contr.SAS", setup = { n <- 10L; n100 <- 100L }, small = contr.SAS(n), n_100 = contr.SAS(n100))
bench("contr.sum", setup = { n <- 10L; n100 <- 100L }, small = contr.sum(n), n_100 = contr.sum(n100))
bench("contr.treatment", setup = { n <- 10L; n100 <- 100L }, small = contr.treatment(n), n_100 = contr.treatment(n100))
bench("contrasts", setup = { f <- df_1e3$g }, fct_1e3 = contrasts(f))
bench("contrasts<-", setup = { f <- df_1e3$g; cs <- contr.sum(10L) }, fct_1e3 = contrasts(f) <- cs)
bench("convolve", setup = { y <- pos_1e3; x5 <- dbl_1e6[1:1e5]; y5 <- pos_1e6[1:1e5] },
      dbl_1e3 = convolve(dbl_1e3, y),
      open_1e3 = convolve(dbl_1e3, y, type = "open"),
      dbl_1e5 = convolve(x5, y5))
bench("cooks.distance", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = cooks.distance(fit))
bench("cophenetic", setup = { hc <- hclust(dist(mat_100)) }, hclust_100 = cophenetic(hc))
bench("cor", setup = { y <- pos_1e3; y6 <- pos_1e6 },
      dbl_1e3 = cor(dbl_1e3, y),
      spearman_1e3 = cor(dbl_1e3, y, method = "spearman"),
      mat_100 = cor(mat_100),
      df_1e3 = cor(df_1e3[c("x", "y")]),
      dbl_1e6 = cor(dbl_1e6, y6))
bench("cor.test", setup = { y <- pos_1e3 },
      dbl_1e3 = cor.test(dbl_1e3, y),
      kendall_1e3 = cor.test(dbl_1e3, y, method = "kendall"))
bench("cov", setup = { y <- pos_1e3; y6 <- pos_1e6 },
      dbl_1e3 = cov(dbl_1e3, y),
      mat_100 = cov(mat_100),
      dbl_1e6 = cov(dbl_1e6, y6))
bench("cov.wt", mat_100 = cov.wt(mat_100))
bench("cov2cor", spd_100 = cov2cor(spd_100))
bench("covratio", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = covratio(fit))
skip("cpgram", "graphics device")
bench("cutree", setup = { hc <- hclust(dist(mat_100)); hc1000 <- hclust(dist(mat_1000[, 1:10])) },
      hclust_100 = cutree(hc, k = 10L),
      hclust_1000 = cutree(hc1000, k = 10L))
bench("cycle", ts_1e3 = cycle(ts_1e3))
bench("D", setup = { e <- quote(sin(x)^2 * exp(x) / (1 + x^2)) }, small = D(e, "x"))
bench("dbeta", scalar = dbeta(dbl_1 / 2, 2, 3), prob_1e3 = dbeta(prob_1e3, 2, 3), prob_1e6 = dbeta(prob_1e6, 2, 3))
bench("dbinom", scalar = dbinom(int_1, 1000L, 0.3), int_1e3 = dbinom(int_1e3, 1000L, 0.3), int_1e6 = dbinom(int_1e6, 1000L, 0.3))
bench("dcauchy", scalar = dcauchy(dbl_1), dbl_1e3 = dcauchy(dbl_1e3), dbl_1e6 = dcauchy(dbl_1e6))
bench("dchisq", scalar = dchisq(dbl_1, 5), pos_1e3 = dchisq(pos_1e3, 5), pos_1e6 = dchisq(pos_1e6, 5))
bench("decompose", ts_1e3 = decompose(ts_1e3))
bench("delete.response", setup = { tt <- terms(formula_1) }, small = delete.response(tt))
bench("deltat", ts_1e3 = deltat(ts_1e3))
bench("dendrapply", setup = { dend <- as.dendrogram(hclust(dist(mat_100))); identity_node <- function(node) node },
      dendrogram_100 = dendrapply(dend, identity_node))
bench("density", dbl_1e3 = density(dbl_1e3), dbl_1e6 = density(dbl_1e6))
bench("density.default", dbl_1e3 = density.default(dbl_1e3))
bench("deriv", setup = { e <- quote(sin(x)^2 * exp(y) / (1 + x^2)) }, small = deriv(e, c("x", "y")))
bench("deriv3", setup = { e <- quote(sin(x)^2 * exp(y) / (1 + x^2)) }, small = deriv3(e, c("x", "y")))
bench("deviance", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = deviance(fit))
bench("dexp", scalar = dexp(dbl_1), pos_1e3 = dexp(pos_1e3, 0.1), pos_1e6 = dexp(pos_1e6, 0.1))
bench("df", scalar = df(dbl_1, 5, 10), pos_1e3 = df(pos_1e3, 5, 10), pos_1e6 = df(pos_1e6, 5, 10))
bench("df.kernel", setup = { k <- kernel("daniell", 5L) }, small = df.kernel(k))
bench("df.residual", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = df.residual(fit))
bench("DF2formula", df_10 = DF2formula(df_10))
bench("dfbeta", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = dfbeta(fit))
bench("dfbetas", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = dfbetas(fit))
bench("dffits", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = dffits(fit))
bench("dgamma", scalar = dgamma(dbl_1, 2), pos_1e3 = dgamma(pos_1e3, 2, 0.1), pos_1e6 = dgamma(pos_1e6, 2, 0.1))
bench("dgeom", scalar = dgeom(int_1, 0.1), int_1e3 = dgeom(int_1e3, 0.01), int_1e6 = dgeom(int_1e6, 0.01))
bench("dhyper", setup = { k <- int_1e3 %% 60L; k5 <- int_1e6[1:1e5] %% 60L },
      scalar = dhyper(int_1, 100L, 200L, 60L),
      int_1e3 = dhyper(k, 100L, 200L, 60L),
      int_1e5 = dhyper(k5, 100L, 200L, 60L))
bench("diffinv", dbl_1e3 = diffinv(dbl_1e3), dbl_1e6 = diffinv(dbl_1e6))
bench("dist", setup = { m <- matrix(dbl_1e6[1:1e4], 1000L) },
      mat_100 = dist(mat_100),
      manhattan_100 = dist(mat_100, "manhattan"),
      mat_1000x10 = dist(m))
bench("dlnorm", scalar = dlnorm(dbl_1), pos_1e3 = dlnorm(pos_1e3), pos_1e6 = dlnorm(pos_1e6))
bench("dlogis", scalar = dlogis(dbl_1), dbl_1e3 = dlogis(dbl_1e3, 0, 10), dbl_1e6 = dlogis(dbl_1e6, 0, 10))
bench("dmultinom", setup = { x <- c(3L, 4L, 5L, 2L); p <- c(0.1, 0.2, 0.3, 0.4) }, small = dmultinom(x, prob = p))
bench("dnbinom", scalar = dnbinom(int_1, 5, 0.3), int_1e3 = dnbinom(int_1e3, 5, 0.01), int_1e6 = dnbinom(int_1e6, 5, 0.01))
bench("dnorm", scalar = dnorm(dbl_1), dbl_1e3 = dnorm(dbl_1e3, 0, 50), dbl_1e6 = dnorm(dbl_1e6, 0, 50),
      log_1e3 = dnorm(dbl_1e3, log = TRUE))
bench("dpois", scalar = dpois(int_1, 5), int_1e3 = dpois(int_1e3, 500), int_1e6 = dpois(int_1e6, 500))
bench("drop.scope", setup = { small <- y ~ x; big <- y ~ x + g + x:g }, small = drop.scope(big, small))
bench("drop.terms", setup = { tt <- terms(y ~ x + g + x:g) }, small = drop.terms(tt, 1L, keep.response = TRUE))
bench("drop1", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = drop1(fit))
bench("dsignrank", setup = { k <- int_1e3 %% 1276L }, scalar = dsignrank(int_1, 50), int_1e3 = dsignrank(k, 50))
bench("dt", scalar = dt(dbl_1, 5), dbl_1e3 = dt(dbl_1e3 / 20, 5), dbl_1e6 = dt(dbl_1e6 / 20, 5))
bench("dummy.coef", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = dummy.coef(fit))
bench("dummy.coef.lm", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = dummy.coef.lm(fit))
bench("dunif", scalar = dunif(dbl_1), dbl_1e3 = dunif(dbl_1e3, -100, 100), dbl_1e6 = dunif(dbl_1e6, -100, 100))
bench("dweibull", scalar = dweibull(dbl_1, 2), pos_1e3 = dweibull(pos_1e3, 2, 50), pos_1e6 = dweibull(pos_1e6, 2, 50))
bench("dwilcox", setup = { k <- int_1e3 %% 1201L }, scalar = dwilcox(int_1, 30, 40), int_1e3 = dwilcox(k, 30, 40))
bench("ecdf", setup = { x5 <- dbl_1e6[1:1e5] }, dbl_1e3 = ecdf(dbl_1e3), dbl_1e5 = ecdf(x5))
bench("eff.aovlist", setup = { fit <- aov(yield ~ N * P * K + Error(block), datasets::npk) },
      aovlist = eff.aovlist(fit))
bench("effects", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = effects(fit))
bench("embed", dbl_1e3 = embed(dbl_1e3, 3L), dbl_1e6 = embed(dbl_1e6, 3L))
bench("end", ts_1e3 = end(ts_1e3))
bench("estVar", setup = { fit <- lm(cbind(y, x) ~ g, model_1e3) }, mlm_1e3 = estVar(fit))
bench("expand.model.frame", setup = { fit <- lm(y ~ x, model_1e3) }, lm_1e3 = expand.model.frame(fit, ~ g))
bench("extractAIC", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = extractAIC(fit))
bench("factanal", setup = { covariance <- datasets::ability.cov }, covmat = factanal(factors = 2L, covmat = covariance))
bench("factor.scope", setup = { factors <- attr(terms(y ~ x + g + x:g), "factors") },
      small = factor.scope(factors, list(drop = factors)))
bench("family", setup = { gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) }, glm_1e3 = family(gfit))
bench("fft",
      dbl_1e3 = fft(dbl_1e3),
      cpl_1e3 = fft(cpl_1e3),
      inverse_1e3 = fft(cpl_1e3, inverse = TRUE),
      dbl_1e6 = fft(dbl_1e6))
bench("filter", setup = { weights <- rep(1 / 5, 5L) },
      dbl_1e3 = filter(dbl_1e3, weights),
      recursive_1e3 = filter(dbl_1e3, 0.5, method = "recursive"),
      dbl_1e6 = filter(dbl_1e6, weights))
bench("fisher.test", setup = { tab <- table(df_1e3$b, df_1e3$x > 0); tab3 <- matrix(c(3L, 1L, 2L, 4L, 5L, 2L, 1L, 3L, 6L), 3L) },
      table_2x2 = fisher.test(tab),
      table_3x3 = fisher.test(tab3))
bench("fitted", setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      lm_1e3 = fitted(fit),
      glm_1e3 = fitted(gfit))
bench("fitted.values", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = fitted.values(fit))
bench("fivenum", dbl_1e3 = fivenum(dbl_1e3), dbl_1e6 = fivenum(dbl_1e6))
bench("fligner.test", df_1e3 = fligner.test(x ~ g, df_1e3))
bench("formula", setup = { fit <- lm(y ~ x + g, model_1e3); text <- "y ~ x + g" },
      lm_1e3 = formula(fit),
      chr = formula(text),
      df_10 = formula(df_10))
skip("free1way", "needs the Matrix package")
bench("frequency", ts_1e3 = frequency(ts_1e3))
bench("friedman.test", setup = { m <- matrix(dbl_1e3[1:100], 20L) }, mat_20x5 = friedman.test(m))
bench("ftable", setup = { titanic <- datasets::Titanic },
      table = ftable(titanic, row.vars = 1:2),
      df_1e3 = ftable(df_1e3[c("g", "b")]))
bench("Gamma", setup = { link <- "log" }, scalar = Gamma(link = link))
bench("gaussian", setup = { link <- "identity" }, scalar = gaussian(link = link))
bench("get_all_vars", df_1e3 = get_all_vars(formula_1, df_1e3))
bench("getCall", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = getCall(fit))
bench("getInitial", setup = { dnase <- subset(datasets::DNase, Run == 1) },
      sslogis = getInitial(density ~ SSlogis(log(conc), Asym, xmid, scal), data = dnase))
bench(".getXlevels", setup = { fit <- lm(y ~ x + g, model_1e3); tt <- terms(fit); mf <- model.frame(fit) },
      lm_1e3 = .getXlevels(tt, mf))
bench("glm", setup = { counts <- data.frame(n = int_1e3, x = dbl_1e3 / 100) },
      binomial_1e3 = glm(y > 3 ~ x + g, binomial, model_1e3),
      gaussian_1e3 = glm(y ~ x + g, gaussian, model_1e3),
      poisson_1e3 = glm(n ~ x, poisson, counts))
bench("glm.control", setup = { epsilon <- 1e-10 }, scalar = glm.control(epsilon = epsilon))
bench("glm.fit", setup = { X <- model.matrix(~ x + g, model_1e3); y <- as.numeric(model_1e3$y > 3); family <- binomial() },
      binomial_1e3 = glm.fit(X, y, family = family))
bench("hasTsp", dbl_1e3 = hasTsp(dbl_1e3), ts_1e3 = hasTsp(ts_1e3))
bench("hat", setup = { X <- model.matrix(~ x + g, model_1e3) }, mat_1e3 = hat(X))
bench("hatvalues", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = hatvalues(fit))
bench("hclust", setup = { d <- dist(mat_100); d1000 <- dist(mat_1000[, 1:10]) },
      dist_100 = hclust(d),
      average_100 = hclust(d, "average"),
      dist_1000 = hclust(d1000))
skip("heatmap", "graphics device")
bench("HoltWinters", setup = { x <- datasets::co2 },
      ts_1e3 = HoltWinters(ts_1e3, gamma = FALSE),
      seasonal_468 = HoltWinters(x))
bench("influence", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = influence(fit))
bench("influence.measures", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = influence.measures(fit))
bench("integrate", setup = { f <- dnorm; g <- function(x) exp(-x) * sin(x) },
      dnorm = integrate(f, -Inf, Inf),
      closure = integrate(g, 0, 10))
skip("interaction.plot", "graphics device")
bench("inverse.gaussian", setup = { link <- "1/mu^2" }, scalar = inverse.gaussian(link = link))
bench("IQR", dbl_1e3 = IQR(dbl_1e3), dbl_1e6 = IQR(dbl_1e6))
bench("is.empty.model", setup = { fit <- lm(y ~ x + g, model_1e3) }, formula = is.empty.model(formula_1), lm_1e3 = is.empty.model(fit))
bench("is.leaf", setup = { dend <- as.dendrogram(hclust(dist(mat_10))) }, dendrogram = is.leaf(dend))
bench("is.mts", ts_1e3 = is.mts(ts_1e3))
bench("is.stepfun", setup = { e <- ecdf(dbl_1e3) }, ecdf_1e3 = is.stepfun(e), dbl_1e3 = is.stepfun(dbl_1e3))
bench("is.ts", ts_1e3 = is.ts(ts_1e3), dbl_1e3 = is.ts(dbl_1e3))
bench("is.tskernel", setup = { k <- kernel("daniell", 5L) }, small = is.tskernel(k))
bench("isoreg", setup = { y <- dbl_1e3; y6 <- dbl_1e6 },
      dbl_1e3 = isoreg(y),
      xy_1e3 = isoreg(dbl_1e3, y),
      dbl_1e6 = isoreg(y6))
bench("KalmanForecast", setup = { model <- arima(ts_1e3, order = c(2L, 0L, 1L))$model },
      arima = KalmanForecast(12L, model))
bench("KalmanLike", setup = { model <- arima(ts_1e3, order = c(2L, 0L, 1L))$model },
      ts_1e3 = KalmanLike(ts_1e3, model))
bench("KalmanRun", setup = { model <- arima(ts_1e3, order = c(2L, 0L, 1L))$model },
      ts_1e3 = KalmanRun(ts_1e3, model))
bench("KalmanSmooth", setup = { model <- arima(ts_1e3, order = c(2L, 0L, 1L))$model },
      ts_1e3 = KalmanSmooth(ts_1e3, model))
bench("kernapply", setup = { k <- kernel("daniell", 5L) },
      dbl_1e3 = kernapply(dbl_1e3, k),
      dbl_1e6 = kernapply(dbl_1e6, k))
bench("kernel", setup = { m <- 5L }, daniell = kernel("daniell", m), modified = kernel("modified.daniell", c(m, m)))
bench("kmeans", setup = { m <- mat_1000[, 1:5] },
      mat_1000x5 = kmeans(m, 5L),
      df_1e3 = kmeans(df_1e3[c("x", "y")], 3L))
bench("knots", setup = { e <- ecdf(dbl_1e3) }, ecdf_1e3 = knots(e))
bench("kruskal.test", df_1e3 = kruskal.test(x ~ g, df_1e3))
bench("ks.test", setup = { y <- pos_1e3; x5 <- unique(dbl_1e6[1:1e5]) },
      one_sample_1e3 = ks.test(dbl_1e3, "punif", -100, 100),
      two_sample_1e3 = ks.test(dbl_1e3, y),
      one_sample_1e5 = ks.test(x5, "punif", -100, 100))
bench("ksmooth", dbl_1e3 = ksmooth(sorted_1e3, dbl_1e3, "normal", bandwidth = 5))
bench("lag", ts_1e3 = lag(ts_1e3, 1L))
skip("lag.plot", "graphics device")
bench("line", dbl_1e3 = line(sorted_1e3, dbl_1e3))
bench("lm", setup = { data10 <- model_1e3[1:10, ]; d5 <- df_1e6[1:1e5, ] },
      small = lm(y ~ x, data10),
      model_1e3 = lm(y ~ x + g, model_1e3),
      df_1e5 = lm(y ~ x + g, d5))
bench(".lm.fit", setup = { X <- model.matrix(~ x + g, model_1e3); y <- model_1e3$y }, mat_1e3 = .lm.fit(X, y))
bench("lm.fit", setup = { X <- model.matrix(~ x + g, model_1e3); y <- model_1e3$y }, mat_1e3 = lm.fit(X, y))
bench("lm.influence", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = lm.influence(fit))
bench("lm.wfit", setup = { X <- model.matrix(~ x + g, model_1e3); y <- model_1e3$y; w <- pos_1e3 }, mat_1e3 = lm.wfit(X, y, w))
bench("loadings", setup = { pc <- princomp(mat_100[, 1:5]) }, princomp = loadings(pc))
bench("loess", loess_1e3 = loess(y ~ x, model_1e3))
bench("loess.control", setup = { surface <- "direct" }, scalar = loess.control(surface = surface))
bench("loess.smooth", dbl_1e3 = loess.smooth(dbl_1e3, pos_1e3))
bench("logLik", setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      lm_1e3 = logLik(fit),
      glm_1e3 = logLik(gfit))
bench("loglin", setup = { tab <- table(df_1e3$g, df_1e3$b, df_1e3$x > 0) },
      table = loglin(tab, list(c(1, 2), 3), print = FALSE))
bench("lowess", setup = { x5 <- sorted_1e6[1:1e5]; y5 <- dbl_1e6[1:1e5] },
      dbl_1e3 = lowess(sorted_1e3, dbl_1e3),
      dbl_1e5 = lowess(x5, y5))
bench("ls.diag", setup = { fit <- lsfit(model.matrix(~ x + g, model_1e3)[, -1L], model_1e3$y) }, lsfit_1e3 = ls.diag(fit))
bench("ls.print", setup = { fit <- lsfit(model.matrix(~ x + g, model_1e3)[, -1L], model_1e3$y) }, lsfit_1e3 = ls.print(fit))
bench("lsfit", setup = { X <- model.matrix(~ x + g, model_1e3)[, -1L]; y <- model_1e3$y }, mat_1e3 = lsfit(X, y))
bench("mad", dbl_1e3 = mad(dbl_1e3), dbl_1e6 = mad(dbl_1e6))
bench("mahalanobis", setup = { m <- matrix(dbl_1e6[1:1e4], 1000L); center <- colMeans(m); covariance <- cov(m) },
      mat_1000x10 = mahalanobis(m, center, covariance))
bench("make.link", setup = { link <- "logit" }, scalar = make.link(link))
bench("makeARIMA", setup = { phi <- c(0.5, -0.2); theta <- 0.3 }, small = makeARIMA(phi, theta, numeric()))
bench("makepredictcall", setup = { v <- poly(sorted_1e3, 2L); call <- quote(poly(x, 2L)) },
      poly_1e3 = makepredictcall(v, call))
bench("manova", mlm_1e3 = manova(cbind(y, x) ~ g, model_1e3))
bench("mantelhaen.test", setup = { tab <- datasets::UCBAdmissions }, table = mantelhaen.test(tab))
bench("mauchly.test", setup = { fit <- lm(matrix(dbl_1e3, ncol = 4L) ~ 1) }, mlm_250 = mauchly.test(fit, X = ~1))
bench("mcnemar.test", setup = { tab <- table(df_1e3$b, df_1e3$x > 0) }, table_2x2 = mcnemar.test(tab))
bench("median",
      scalar = median(dbl_1),
      dbl_1e3 = median(dbl_1e3),
      int_1e3 = median(int_1e3),
      na_1e3 = median(na_1e3, na.rm = TRUE),
      dbl_1e6 = median(dbl_1e6))
bench("median.default", dbl_1e3 = median.default(dbl_1e3), dbl_1e6 = median.default(dbl_1e6))
bench("medpolish", mat_100 = medpolish(mat_100))
bench(".MFclass", dbl_1e3 = .MFclass(dbl_1e3), fct_1e3 = .MFclass(fct_1e3))
bench("model.extract", setup = { mf <- model.frame(formula_1, df_1e3) }, df_1e3 = model.extract(mf, "response"))
bench("model.frame",
      df_1e3 = model.frame(formula_1, df_1e3),
      df_1e6 = model.frame(formula_1, df_1e6))
bench("model.frame.default", df_1e3 = model.frame.default(formula_1, df_1e3))
bench("model.matrix",
      df_1e3 = model.matrix(formula_1, df_1e3),
      df_1e6 = model.matrix(formula_1, df_1e6))
bench("model.matrix.default", df_1e3 = model.matrix.default(formula_1, df_1e3))
bench("model.matrix.lm", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = model.matrix.lm(fit))
bench("model.offset", setup = { mf <- model.frame(y ~ x + offset(log(pos_1e3)), df_1e3) }, df_1e3 = model.offset(mf))
bench("model.response", setup = { mf <- model.frame(formula_1, df_1e3) }, df_1e3 = model.response(mf))
bench("model.tables", setup = { fit <- aov(yield ~ N + P + K, datasets::npk) }, aov = model.tables(fit))
bench("model.weights", setup = { d <- df_1e3; d$w <- pos_1e3; mf <- model.frame(formula_1, d, weights = w) }, df_1e3 = model.weights(mf))
skip("monthplot", "graphics device")
bench("mood.test", setup = { y <- pos_1e3 }, dbl_1e3 = mood.test(dbl_1e3, y))
bench("mvfft", mat_100 = mvfft(mat_100), mat_1000 = mvfft(mat_1000))
bench("na.action", setup = { omitted <- na.omit(data.frame(x = na_1e3)) }, df_1e3 = na.action(omitted))
bench("na.contiguous", na_1e3 = na.contiguous(na_1e3))
bench("na.exclude", setup = { d <- data.frame(x = na_1e3, y = dbl_1e3) }, df_1e3 = na.exclude(d))
bench("na.fail", df_1e3 = na.fail(df_1e3))
