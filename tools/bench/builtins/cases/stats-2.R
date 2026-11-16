bench("na.omit",
      setup = { d <- df_1e3; d$x[seq(1L, 1000L, 10L)] <- NA },
      na_1e3 = na.omit(na_1e3), na_1e6 = na.omit(na_1e6), df_1e3 = na.omit(d))
bench("na.pass", dbl_1e3 = na.pass(na_1e3), df_10 = na.pass(df_10))
bench("napredict",
      setup = { kept <- na.exclude(na_1e3); action <- attr(kept, "na.action"); v <- as.numeric(kept) },
      na_1e3 = napredict(action, v))
bench("naprint", setup = { action <- attr(na.omit(na_1e3), "na.action") }, na_1e3 = naprint(action))
bench("naresid",
      setup = { kept <- na.exclude(na_1e3); action <- attr(kept, "na.action"); v <- as.numeric(kept) },
      na_1e3 = naresid(action, v))
bench("nextn", scalar = nextn(int_1), int_1e3 = nextn(int_1e3))
bench(".nknots.smspl", setup = { n <- 1000L }, scalar = .nknots.smspl(n))
bench("nlm",
      setup = { f <- function(x) sum((x - c(1, 2))^2) + (x[1L] * x[2L] - 2)^2; p <- c(10, 10) },
      small = nlm(f, p))
bench("nlminb",
      setup = { f <- function(x) (1 - x[1L])^2 + 100 * (x[2L] - x[1L]^2)^2; p <- c(-1.2, 1) },
      small = nlminb(p, f))
bench("nls",
      setup = { x <- 1:50; d <- data.frame(x = x, y = 3 * exp(-0.1 * x) + rnorm(50L, sd = 0.01)) },
      small = nls(y ~ a * exp(-b * x), d, start = list(a = 1, b = 0.05)))
bench("nls.control", setup = { n <- 100L }, scalar = nls.control(maxiter = n))
bench("NLSstAsymptotic",
      setup = { x <- 1:20; xy <- sortedXyData(x, 5 * (1 - exp(-0.3 * x)) + rnorm(20L, sd = 0.01)) },
      small = NLSstAsymptotic(xy))
bench("NLSstClosestX",
      setup = { x <- 1:20; xy <- sortedXyData(x, 5 * (1 - exp(-0.3 * x)) + rnorm(20L, sd = 0.01)); y0 <- 2.5 },
      small = NLSstClosestX(xy, y0))
bench("NLSstLfAsymptote",
      setup = { x <- 1:20; xy <- sortedXyData(x, 5 * (1 - exp(-0.3 * x)) + rnorm(20L, sd = 0.01)) },
      small = NLSstLfAsymptote(xy))
bench("NLSstRtAsymptote",
      setup = { x <- 1:20; xy <- sortedXyData(x, 5 * (1 - exp(-0.3 * x)) + rnorm(20L, sd = 0.01)) },
      small = NLSstRtAsymptote(xy))
bench("nobs", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = nobs(fit))
bench("numericDeriv",
      setup = { e <- new.env(); e$a <- 3; e$b <- 0.1; e$x <- 1:100; model <- quote(a * exp(-b * x)) },
      small = numericDeriv(model, c("a", "b"), e))
bench("offset", dbl_1e3 = offset(dbl_1e3), dbl_1e6 = offset(dbl_1e6))
bench("oneway.test", model_1e3 = oneway.test(y ~ g, model_1e3))
bench("optim",
      setup = { f <- function(x) (1 - x[1L])^2 + 100 * (x[2L] - x[1L]^2)^2; p <- c(-1.2, 1) },
      nelder_mead = optim(p, f), bfgs = optim(p, f, method = "BFGS"),
      lbfgsb = optim(p, f, method = "L-BFGS-B", lower = -2, upper = 2))
bench("optimHess",
      setup = { f <- function(x) (1 - x[1L])^2 + 100 * (x[2L] - x[1L]^2)^2; p <- c(1, 1) },
      small = optimHess(p, f))
bench("optimise", setup = { f <- function(x) (x - 2)^2; interval <- c(0, 10) }, small = optimise(f, interval))
bench("optimize", setup = { f <- function(x) (x - 2)^2; interval <- c(0, 10) }, small = optimize(f, interval))
bench("order.dendrogram", setup = { d <- as.dendrogram(hclust(dist(mat_100))) }, mat_100 = order.dendrogram(d))
bench("p.adjust", prob_1e3 = p.adjust(prob_1e3, "BH"), holm_1e3 = p.adjust(prob_1e3, "holm"),
      bonferroni_1e3 = p.adjust(prob_1e3, "bonferroni"), prob_1e6 = p.adjust(prob_1e6, "BH"))
bench("pacf", ts_1e3 = pacf(ts_1e3, plot = FALSE))
bench("Pair", dbl_1e3 = Pair(dbl_1e3, pos_1e3))
bench("pairwise.prop.test", setup = { x <- c(30, 40, 50, 60); n <- c(100, 100, 100, 100) },
      small = pairwise.prop.test(x, n))
bench("pairwise.t.test", model_1e3 = pairwise.t.test(model_1e3$y, model_1e3$g))
bench("pairwise.table", setup = { levels <- letters[1:6]; compare <- function(i, j) abs(i - j) / 10 },
      small = pairwise.table(compare, levels, "holm"))
bench("pairwise.wilcox.test", model_1e3 = pairwise.wilcox.test(model_1e3$y, model_1e3$g))
bench("pbeta", scalar = pbeta(dbl_1, 2, 3), prob_1e3 = pbeta(prob_1e3, 2, 3), prob_1e6 = pbeta(prob_1e6, 2, 3))
bench("pbinom", setup = { q <- int_1e3 %/% 20L; q6 <- int_1e6 %/% 20L },
      scalar = pbinom(int_1, 20, 0.3), int_1e3 = pbinom(q, 50, 0.3), int_1e6 = pbinom(q6, 50, 0.3))
bench("pbirthday", setup = { n <- 23 }, scalar = pbirthday(n))
bench("pcauchy", dbl_1e3 = pcauchy(dbl_1e3), dbl_1e6 = pcauchy(dbl_1e6))
bench("pchisq", scalar = pchisq(dbl_1, 3), pos_1e3 = pchisq(pos_1e3, 3), pos_1e6 = pchisq(pos_1e6, 3),
      upper_1e3 = pchisq(pos_1e3, 10, lower.tail = FALSE))
bench("pexp", scalar = pexp(dbl_1), pos_1e3 = pexp(pos_1e3, 0.1), pos_1e6 = pexp(pos_1e6, 0.1))
bench("pf", scalar = pf(dbl_1, 3, 20), pos_1e3 = pf(pos_1e3, 3, 20), pos_1e6 = pf(pos_1e6, 3, 20))
bench("pgamma", scalar = pgamma(dbl_1, 2), pos_1e3 = pgamma(pos_1e3, 2, 0.1), pos_1e6 = pgamma(pos_1e6, 2, 0.1))
bench("pgeom", scalar = pgeom(int_1, 0.2), int_1e3 = pgeom(int_1e3, 0.01), int_1e6 = pgeom(int_1e6, 0.01))
bench("phyper", setup = { q <- int_1e3 %/% 50L; q6 <- int_1e6 %/% 50L },
      scalar = phyper(int_1, 20, 30, 15), int_1e3 = phyper(q, 20, 30, 15), int_1e6 = phyper(q6, 20, 30, 15))
skip("plclust", "defunct")
bench("plnorm", pos_1e3 = plnorm(pos_1e3), pos_1e6 = plnorm(pos_1e6))
bench("plogis", scalar = plogis(dbl_1), dbl_1e3 = plogis(dbl_1e3), dbl_1e6 = plogis(dbl_1e6))
skip("plot.ecdf", "graphics device")
skip("plot.spec.coherency", "graphics device")
skip("plot.spec.phase", "graphics device")
skip("plot.stepfun", "graphics device")
skip("plot.ts", "graphics device")
bench("pnbinom", setup = { q <- int_1e3 %/% 20L; q6 <- int_1e6 %/% 20L },
      scalar = pnbinom(int_1, 5, 0.3), int_1e3 = pnbinom(q, 5, 0.3), int_1e6 = pnbinom(q6, 5, 0.3))
bench("pnorm", scalar = pnorm(dbl_1), dbl_1e3 = pnorm(dbl_1e3, 0, 50), dbl_1e6 = pnorm(dbl_1e6, 0, 50),
      upper_1e3 = pnorm(dbl_1e3, 0, 50, lower.tail = FALSE), log_1e3 = pnorm(dbl_1e3, 0, 50, log.p = TRUE))
bench("poisson", setup = { link <- "log" }, scalar = poisson(link))
bench("poisson.test", setup = { x <- 137; t <- 24.19893 }, scalar = poisson.test(x, t))
bench("poly", setup = { x5 <- dbl_1e6[seq_len(1e5)] }, dbl_1e3 = poly(dbl_1e3, 3), dbl_1e5 = poly(x5, 3), raw_1e3 = poly(dbl_1e3, 3, raw = TRUE))
bench("polym", dbl_1e3 = polym(dbl_1e3, pos_1e3, degree = 2))
bench("power", setup = { lambda <- 0.5 }, scalar = power(lambda))
bench("power.anova.test", setup = { groups <- 4 },
      small = power.anova.test(groups = groups, n = 5, between.var = 1, within.var = 3))
skip("power.free1way.test", "needs the Matrix package")
bench("power.prop.test", setup = { n <- 50 }, small = power.prop.test(n = n, p1 = 0.5, p2 = 0.75),
      solve_n = power.prop.test(p1 = 0.5, p2 = 0.75, power = 0.9))
bench("power.t.test", setup = { n <- 20 }, small = power.t.test(n = n, delta = 1),
      solve_n = power.t.test(delta = 1, power = 0.9, sd = n / 20))
bench("PP.test", ts_1e3 = PP.test(ts_1e3))
bench("ppoints", setup = { n <- 1000L }, scalar = ppoints(n), dbl_1e3 = ppoints(dbl_1e3))
bench("ppois", scalar = ppois(int_1, 5), int_1e3 = ppois(int_1e3, 500), int_1e6 = ppois(int_1e6, 500))
bench("ppplot", dbl_1e3 = ppplot(dbl_1e3, pos_1e3, plot.it = FALSE))
bench("ppr",
      setup = { d <- data.frame(x1 = runif(500L), x2 = runif(500L)); d$y <- sin(3 * d$x1) + d$x2^2 + rnorm(500L, sd = 0.1) },
      small = ppr(y ~ x1 + x2, d, nterms = 2, max.terms = 3))
bench("prcomp", mat_100 = prcomp(mat_100), df_1e3 = prcomp(df_1e3[c("x", "y")], scale. = TRUE))
bench("predict",
      setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3); new <- model_1e3[1:100, ] },
      lm_1e3 = predict(fit), lm_new = predict(fit, new), glm_new = predict(gfit, new, type = "response"),
      interval = predict(fit, new, interval = "confidence"))
bench("predict.glm",
      setup = { gfit <- glm(y > 3 ~ x + g, binomial, model_1e3); new <- model_1e3[1:100, ] },
      glm_1e3 = predict.glm(gfit), glm_new = predict.glm(gfit, new, type = "response", se.fit = TRUE))
bench("predict.lm", setup = { fit <- lm(y ~ x + g, model_1e3); new <- model_1e3[1:100, ] },
      lm_1e3 = predict.lm(fit), lm_new = predict.lm(fit, new, se.fit = TRUE))
bench(".preformat.ts", ts_1e3 = .preformat.ts(ts_1e3))
skip("preplot", "generic with no method in base R")
bench("princomp", setup = { m <- matrix(rnorm(5000L), 1000L, 5L) }, mat_1e3 = princomp(m))
bench("printCoefmat", setup = { table <- coef(summary(lm(y ~ x + g, model_1e3))) }, small = printCoefmat(table))
bench("profile",
      setup = { x <- 1:50; d <- data.frame(x = x, y = 3 * exp(-0.1 * x) + rnorm(50L, sd = 0.05));
                fit <- nls(y ~ a * exp(-b * x), d, start = list(a = 1, b = 0.05)) },
      nls_small = profile(fit))
bench("proj", setup = { fit <- aov(y ~ x + g, model_1e3) }, aov_1e3 = proj(fit))
bench("promax", setup = { m <- matrix(runif(30L), 10L, 3L) }, small = promax(m))
bench("prop.test", setup = { x <- c(83, 90); n <- c(186, 193) }, small = prop.test(x, n),
      one = prop.test(x[1L], n[1L]))
bench("prop.trend.test", setup = { x <- c(83, 90, 129, 70); n <- c(186, 193, 236, 182) },
      small = prop.trend.test(x, n))
bench("psignrank", setup = { q <- 0:210; n <- 20 }, small = psignrank(q, n))
bench("psmirnov", setup = { q <- seq(0, 1, by = 0.05) }, small = psmirnov(q, sizes = c(30, 40)))
bench("pt", scalar = pt(dbl_1, 5), dbl_1e3 = pt(dbl_1e3 / 20, 5), dbl_1e6 = pt(dbl_1e6 / 20, 5))
bench("ptukey", setup = { q <- seq(0.1, 5, length.out = 50L) }, small = ptukey(q, 3, 20))
bench("punif", dbl_1e3 = punif(dbl_1e3, -100, 100), dbl_1e6 = punif(dbl_1e6, -100, 100))
bench("pweibull", pos_1e3 = pweibull(pos_1e3, 2, 50), pos_1e6 = pweibull(pos_1e6, 2, 50))
bench("pwilcox", setup = { q <- 0:2500 }, small = pwilcox(q, 50, 50))
bench("qbeta", setup = { p5 <- prob_1e6[seq_len(1e5)] }, scalar = qbeta(dbl_1, 2, 3), prob_1e3 = qbeta(prob_1e3, 2, 3), prob_1e5 = qbeta(p5, 2, 3))
bench("qbinom", scalar = qbinom(dbl_1, 20, 0.3), prob_1e3 = qbinom(prob_1e3, 50, 0.3), prob_1e6 = qbinom(prob_1e6, 50, 0.3))
bench("qbirthday", setup = { p <- 0.5 }, scalar = qbirthday(p))
bench("qcauchy", prob_1e3 = qcauchy(prob_1e3), prob_1e6 = qcauchy(prob_1e6))
bench("qchisq", setup = { p5 <- prob_1e6[seq_len(1e5)] }, scalar = qchisq(dbl_1, 3), prob_1e3 = qchisq(prob_1e3, 3), prob_1e5 = qchisq(p5, 3))
bench("qexp", scalar = qexp(dbl_1), prob_1e3 = qexp(prob_1e3, 0.1), prob_1e6 = qexp(prob_1e6, 0.1))
bench("qf", setup = { p5 <- prob_1e6[seq_len(1e5)] }, scalar = qf(dbl_1, 3, 20), prob_1e3 = qf(prob_1e3, 3, 20), prob_1e5 = qf(p5, 3, 20))
bench("qgamma", setup = { p5 <- prob_1e6[seq_len(1e5)] }, scalar = qgamma(dbl_1, 2), prob_1e3 = qgamma(prob_1e3, 2, 0.1), prob_1e5 = qgamma(p5, 2, 0.1))
bench("qgeom", scalar = qgeom(dbl_1, 0.2), prob_1e3 = qgeom(prob_1e3, 0.01), prob_1e6 = qgeom(prob_1e6, 0.01))
bench("qhyper", prob_1e3 = qhyper(prob_1e3, 20, 30, 15), prob_1e6 = qhyper(prob_1e6, 20, 30, 15))
bench("qlnorm", prob_1e3 = qlnorm(prob_1e3), prob_1e6 = qlnorm(prob_1e6))
bench("qlogis", prob_1e3 = qlogis(prob_1e3), prob_1e6 = qlogis(prob_1e6))
bench("qnbinom", scalar = qnbinom(dbl_1, 5, 0.3), prob_1e3 = qnbinom(prob_1e3, 5, 0.3), prob_1e6 = qnbinom(prob_1e6, 5, 0.3))
bench("qnorm", scalar = qnorm(dbl_1), prob_1e3 = qnorm(prob_1e3), prob_1e6 = qnorm(prob_1e6),
      upper_1e3 = qnorm(prob_1e3, 10, 2, lower.tail = FALSE))
bench("qpois", scalar = qpois(dbl_1, 5), prob_1e3 = qpois(prob_1e3, 500), prob_1e6 = qpois(prob_1e6, 500))
skip("qqline", "graphics device")
bench("qqnorm", dbl_1e3 = qqnorm(dbl_1e3, plot.it = FALSE), dbl_1e6 = qqnorm(dbl_1e6, plot.it = FALSE))
bench("qqplot", dbl_1e3 = qqplot(dbl_1e3, pos_1e3, plot.it = FALSE))
bench("qr.influence", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = qr.influence(fit$qr, fit$residuals))
bench("qsignrank", prob_1e3 = qsignrank(prob_1e3, 20))
bench("qsmirnov", setup = { p <- c(0.1, 0.5, 0.9, 0.95) }, small = qsmirnov(p, sizes = c(30, 40)))
bench("qt", setup = { p5 <- prob_1e6[seq_len(1e5)] }, scalar = qt(dbl_1, 5), prob_1e3 = qt(prob_1e3, 5), prob_1e5 = qt(p5, 5))
bench("qtukey", setup = { p <- c(0.5, 0.9, 0.95, 0.99) }, small = qtukey(p, 3, 20))
bench("quade.test", setup = { m <- matrix(runif(50L), 10L, 5L) }, small = quade.test(m))
bench("quantile", scalar = quantile(dbl_1), dbl_1e3 = quantile(dbl_1e3), dbl_1e6 = quantile(dbl_1e6),
      probs_1e3 = quantile(dbl_1e3, c(0.01, 0.05, 0.95, 0.99)), type1_1e3 = quantile(dbl_1e3, type = 1),
      na_1e3 = quantile(na_1e3, na.rm = TRUE), int_1e3 = quantile(int_1e3, 0.9, names = FALSE))
bench("quasi", setup = { link <- "log" }, scalar = quasi(link = link, variance = "mu"))
bench("quasibinomial", setup = { link <- "logit" }, scalar = quasibinomial(link))
bench("quasipoisson", setup = { link <- "log" }, scalar = quasipoisson(link))
bench("qunif", prob_1e3 = qunif(prob_1e3, -1, 1), prob_1e6 = qunif(prob_1e6, -1, 1))
bench("qweibull", prob_1e3 = qweibull(prob_1e3, 2, 50), prob_1e6 = qweibull(prob_1e6, 2, 50))
bench("qwilcox", prob_1e3 = qwilcox(prob_1e3, 20, 20))
bench("r2dtable", setup = { r <- c(10, 20, 30); c <- c(15, 15, 30) }, small = r2dtable(100L, r, c))
bench("rbeta", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rbeta(n, 2, 3), n_1e6 = rbeta(n6, 2, 3))
bench("rbinom", setup = { n <- 1000L; n6 <- 1e6 }, scalar = rbinom(1L, int_1, 0.3), n_1e3 = rbinom(n, 50, 0.3),
      n_1e6 = rbinom(n6, 50, 0.3), prob_1e3 = rbinom(n, 1, prob_1e3))
bench("rcauchy", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rcauchy(n), n_1e6 = rcauchy(n6))
bench("rchisq", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rchisq(n, 3), n_1e6 = rchisq(n6, 3))
skip("read.ftable", "reads files")
skip("rect.hclust", "graphics device")
bench("reformulate", setup = { terms <- c("x", "g", "x:g") }, small = reformulate(terms, "y"))
bench("relevel", fct_1e3 = relevel(fct_1e3, "c"), fct_1e6 = relevel(fct_1e6, "c"))
bench("reorder", fct_1e3 = reorder(fct_1e3, dbl_1e3), fct_1e6 = reorder(fct_1e6, dbl_1e6))
bench("replications", model_1e3 = replications(~ g, model_1e3))
bench("reshape",
      setup = { wide <- data.frame(id = 1:1000, x1 = runif(1000L), x2 = runif(1000L), x3 = runif(1000L));
                long <- reshape(wide, direction = "long", varying = c("x1", "x2", "x3"), v.names = "x",
                                idvar = "id", timevar = "time") },
      to_long_1e3 = reshape(wide, direction = "long", varying = c("x1", "x2", "x3"), v.names = "x",
                            idvar = "id", timevar = "time"),
      to_wide_1e3 = reshape(long, direction = "wide", idvar = "id", timevar = "time", v.names = "x"))
bench("resid", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = resid(fit))
bench("residuals", setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      lm_1e3 = residuals(fit), glm_1e3 = residuals(gfit), pearson_1e3 = residuals(gfit, "pearson"))
bench("residuals.glm", setup = { gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      glm_1e3 = residuals.glm(gfit), pearson_1e3 = residuals.glm(gfit, "pearson"))
bench("residuals.lm", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = residuals.lm(fit))
bench("rexp", setup = { n <- 1000L; n6 <- 1e6 }, scalar = rexp(int_1), n_1e3 = rexp(n, 0.1), n_1e6 = rexp(n6, 0.1))
bench("rf", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rf(n, 3, 20), n_1e6 = rf(n6, 3, 20))
bench("rfree1way", setup = { n <- 50L }, small = rfree1way(n, delta = c(0, 0.5)))
bench("rgamma", setup = { n <- 1000L; n6 <- 1e6 }, scalar = rgamma(int_1, 2), n_1e3 = rgamma(n, 2, 0.1),
      n_1e6 = rgamma(n6, 2, 0.1), small_shape_1e3 = rgamma(n, 0.5))
bench("rgeom", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rgeom(n, 0.01), n_1e6 = rgeom(n6, 0.01))
bench("rhyper", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rhyper(n, 20, 30, 15), n_1e6 = rhyper(n6, 20, 30, 15))
bench("rlnorm", setup = { n <- 1000L; n6 <- 1e6 }, scalar = rlnorm(int_1), n_1e3 = rlnorm(n), n_1e6 = rlnorm(n6))
bench("rlogis", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rlogis(n), n_1e6 = rlogis(n6))
bench("rmultinom", setup = { n <- 1000L; prob <- c(0.1, 0.2, 0.3, 0.4) }, n_1e3 = rmultinom(n, 20, prob))
bench("rnbinom", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rnbinom(n, 5, 0.3), n_1e6 = rnbinom(n6, 5, 0.3),
      mu_1e3 = rnbinom(n, size = 2, mu = 10))
bench("rnorm", setup = { n <- 1000L; n6 <- 1e6 }, scalar = rnorm(int_1), n_1e3 = rnorm(n), n_1e6 = rnorm(n6),
      mean_sd_1e3 = rnorm(n, 10, 2), vector_mean_1e3 = rnorm(n, dbl_1e3, 1))
bench("rpois", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rpois(n, 5), n_1e6 = rpois(n6, 5),
      large_mu_1e3 = rpois(n, 500), vector_mu_1e3 = rpois(n, pos_1e3))
bench("rsignrank", setup = { n <- 1000L }, n_1e3 = rsignrank(n, 20))
bench("rsmirnov", setup = { n <- 100L }, small = rsmirnov(n, sizes = c(30, 40)))
bench("rstandard", setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      lm_1e3 = rstandard(fit), glm_1e3 = rstandard(gfit))
bench("rstudent", setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      lm_1e3 = rstudent(fit), glm_1e3 = rstudent(gfit))
bench("rt", setup = { n <- 1000L; n6 <- 1e6 }, scalar = rt(int_1, 5), n_1e3 = rt(n, 5), n_1e6 = rt(n6, 5))
bench("runif", setup = { n <- 1000L; n6 <- 1e6 }, scalar = runif(int_1), n_1e3 = runif(n), n_1e6 = runif(n6),
      range_1e3 = runif(n, -100, 100), vector_max_1e3 = runif(n, 0, pos_1e3))
bench("runmed", dbl_1e3 = runmed(dbl_1e3, 5), dbl_1e6 = runmed(dbl_1e6, 11), wide_1e3 = runmed(dbl_1e3, 101))
bench("rweibull", setup = { n <- 1000L; n6 <- 1e6 }, n_1e3 = rweibull(n, 2, 50), n_1e6 = rweibull(n6, 2, 50))
bench("rwilcox", setup = { n <- 1000L }, n_1e3 = rwilcox(n, 20, 20))
bench("rWishart", setup = { n <- 100L; sigma <- diag(5) }, small = rWishart(n, 10, sigma))
skip("scatter.smooth", "graphics device")
skip("screeplot", "graphics device")
bench("sd", scalar = sd(c(dbl_1, 1, 2)), dbl_1e3 = sd(dbl_1e3), dbl_1e6 = sd(dbl_1e6),
      na_1e3 = sd(na_1e3, na.rm = TRUE), int_1e3 = sd(int_1e3))
bench("se.contrast", setup = { fit <- aov(y ~ g, model_1e3) },
      aov_1e3 = se.contrast(fit, list(g == "a", g == "b"), data = model_1e3))
bench("selfStart",
      setup = { model <- function(x, a, b) a * exp(-b * x);
                initial <- function(mCall, data, LHS, ...) c(a = 1, b = 0.1) },
      small = selfStart(model, initial, c("a", "b")))
bench("setNames", scalar = setNames(dbl_1, chr_1), dbl_1e3 = setNames(dbl_1e3, chr_1e3),
      dbl_1e6 = setNames(dbl_1e6, chr_1e6), lst_1e3 = setNames(lst_1e3, chr_1e3),
      nm_1e3 = setNames(nm = chr_1e3), df_10 = setNames(df_10, c("a", "b", "c", "d")))
bench("shapiro.test", dbl_1e3 = shapiro.test(dbl_1e3))
bench("sigma", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = sigma(fit))
bench("simulate", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = simulate(fit, nsim = 1L))
bench("smooth", dbl_1e3 = smooth(dbl_1e3), dbl_1e6 = smooth(dbl_1e6))
bench("smooth.spline", model_1e3 = smooth.spline(model_1e3$x, model_1e3$y),
      df_1e3 = smooth.spline(model_1e3$x, model_1e3$y, df = 5))
bench("smoothEnds", dbl_1e3 = smoothEnds(dbl_1e3), dbl_1e6 = smoothEnds(dbl_1e6, 5))
bench("sortedXyData", dbl_1e3 = sortedXyData(dbl_1e3, pos_1e3))
bench("spec.ar", ts_1e3 = spec.ar(ts_1e3, plot = FALSE))
bench("spec.pgram", ts_1e3 = spec.pgram(ts_1e3, plot = FALSE), smoothed_1e3 = spec.pgram(ts_1e3, spans = c(3, 5), plot = FALSE))
bench("spec.taper", dbl_1e3 = spec.taper(dbl_1e3), ts_1e3 = spec.taper(ts_1e3, 0.2))
bench("spectrum", ts_1e3 = spectrum(ts_1e3, plot = FALSE))
bench("spline", sorted_1e3 = spline(sorted_1e3, dbl_1e3), n_1e3 = spline(sorted_1e3, dbl_1e3, n = 5000L))
bench("splinefun", setup = { f <- splinefun(sorted_1e3, dbl_1e3) },
      sorted_1e3 = splinefun(sorted_1e3, dbl_1e3), natural_1e3 = splinefun(sorted_1e3, dbl_1e3, method = "natural"),
      evaluate_1e3 = f(dbl_1e3), evaluate_1e6 = f(dbl_1e6))
bench("splinefunH", setup = { x <- sorted_1e3; y <- sin(x / 10); m <- cos(x / 10) / 10 },
      sorted_1e3 = splinefunH(x, y, m))
bench("SSasymp", pos_1e3 = SSasymp(pos_1e3, 5, 1, -1), pos_1e6 = SSasymp(pos_1e6, 5, 1, -1))
bench("SSasympOff", pos_1e3 = SSasympOff(pos_1e3, 5, -1, 0.5))
bench("SSasympOrig", pos_1e3 = SSasympOrig(pos_1e3, 5, -1))
bench("SSbiexp", pos_1e3 = SSbiexp(pos_1e3, 3, 1, 0.6, -1.5))
bench("SSD", setup = { fit <- lm(cbind(y, x) ~ g, model_1e3) }, mlm_1e3 = SSD(fit))
bench("SSfol", setup = { dose <- 4.02 }, pos_1e3 = SSfol(dose, pos_1e3, -2.5, 0.5, -3))
bench("SSfpl", pos_1e3 = SSfpl(pos_1e3, 1, 10, 50, 5))
bench("SSgompertz", pos_1e3 = SSgompertz(pos_1e3, 10, 2, 0.9))
bench("SSlogis", pos_1e3 = SSlogis(pos_1e3, 10, 50, 5), pos_1e6 = SSlogis(pos_1e6, 10, 50, 5))
bench("SSmicmen", pos_1e3 = SSmicmen(pos_1e3, 200, 10))
bench("SSweibull", pos_1e3 = SSweibull(pos_1e3, 10, 8, -3, 2))
bench("start", ts_1e3 = start(ts_1e3), dbl_1e3 = start(dbl_1e3))
bench("stat.anova",
      setup = { table <- data.frame(Df = c(NA, 1, 3), Deviance = c(NA, 10, 5), `Resid. Df` = c(999, 998, 995),
                                    `Resid. Dev` = c(100, 90, 85), check.names = FALSE) },
      small = stat.anova(table, "Chisq", 1, 995, 1000))
bench("step",
      setup = { d <- model_1e3; d$z <- runif(1000L); fit <- lm(y ~ x + g + z, d) },
      lm_1e3 = step(fit, trace = 0))
bench("stepfun", setup = { y <- c(0, dbl_1e3) }, sorted_1e3 = stepfun(sorted_1e3, y))
bench("stl", ts_1e3 = stl(ts_1e3, "periodic"), window_1e3 = stl(ts_1e3, 13))
bench("StructTS", setup = { x <- window(ts_1e3, end = c(1999, 12)) }, level_small = StructTS(x, "level"))
bench("summary.aov", setup = { fit <- aov(y ~ x + g, model_1e3) }, aov_1e3 = summary.aov(fit))
bench("summary.glm", setup = { gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) }, glm_1e3 = summary.glm(gfit))
bench("summary.lm", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = summary.lm(fit))
bench("summary.manova", setup = { fit <- manova(cbind(y, x) ~ g, model_1e3) }, mlm_1e3 = summary.manova(fit))
bench("summary.stepfun", setup = { f <- stepfun(sorted_1e3, c(0, dbl_1e3)) }, sorted_1e3 = summary.stepfun(f))
bench("supsmu", model_1e3 = supsmu(model_1e3$x, model_1e3$y))
bench("symnum",
      setup = { cuts <- c(0, 0.001, 0.01, 0.05, 0.1, 1); symbols <- c("***", "**", "*", ".", " ") },
      prob_1e3 = symnum(prob_1e3, corr = FALSE, na = FALSE, cutpoints = cuts, symbols = symbols),
      cor_10 = symnum(cor(mat_10)))
bench("t.test", dbl_1e3 = t.test(dbl_1e3), two_1e3 = t.test(dbl_1e3, pos_1e3),
      formula_1e3 = t.test(x ~ b, df_1e3), dbl_1e6 = t.test(dbl_1e6))
skip("termplot", "graphics device")
bench("terms", setup = { fit <- lm(y ~ x + g, model_1e3) }, formula = terms(formula_1), lm_1e3 = terms(fit))
bench("terms.formula", formula = terms.formula(formula_1),
      dot = terms.formula(y ~ ., data = model_1e3))
bench("time", ts_1e3 = time(ts_1e3))
bench("toeplitz", setup = { x <- dbl_1e3[1:100] }, small = toeplitz(x))
bench("toeplitz2", setup = { x <- dbl_1e3[1:150] }, small = toeplitz2(x, ncol = 50L))
bench("ts", dbl_1e3 = ts(dbl_1e3, frequency = 12, start = c(1990, 1)), dbl_1e6 = ts(dbl_1e6),
      mat_100 = ts(mat_100, frequency = 4))
bench("ts.intersect", setup = { a <- window(ts_1e3, start = c(2000, 1)) }, ts_1e3 = ts.intersect(ts_1e3, a))
skip("ts.plot", "graphics device")
bench("ts.union", setup = { a <- window(ts_1e3, start = c(2000, 1)) }, ts_1e3 = ts.union(ts_1e3, a))
skip("tsdiag", "graphics device")
bench("tsp", ts_1e3 = tsp(ts_1e3))
bench("tsp<-", setup = { y <- dbl_1e3; value <- c(1, 1000, 1) }, dbl_1e3 = tsp(y) <- value)
bench("tsSmooth", setup = { fit <- StructTS(window(ts_1e3, end = c(1999, 12)), "level") }, small = tsSmooth(fit))
bench("TukeyHSD", setup = { fit <- aov(y ~ g, model_1e3) }, aov_1e3 = TukeyHSD(fit))
bench("uniroot", setup = { f <- function(x) x^3 - x - 1; interval <- c(1, 2) }, small = uniroot(f, interval),
      tight = uniroot(f, interval, tol = 1e-12))
bench("update", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = update(fit, . ~ . - g),
      formula = update(formula_1, . ~ . + z))
bench("update.default", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = update.default(fit, . ~ x))
bench("update.formula", formula = update.formula(formula_1, . ~ . + z), lhs = update.formula(formula_1, log(.) ~ .))
bench("var", dbl_1e3 = var(dbl_1e3), dbl_1e6 = var(dbl_1e6), two_1e3 = var(dbl_1e3, pos_1e3),
      mat_100 = var(mat_100), na_1e3 = var(na_1e3, na.rm = TRUE))
bench("var.test", dbl_1e3 = var.test(dbl_1e3, pos_1e3))
bench("variable.names", setup = { fit <- lm(y ~ x + g, model_1e3) }, lm_1e3 = variable.names(fit))
bench("varimax", setup = { m <- matrix(runif(30L), 10L, 3L) }, small = varimax(m))
bench("vcov", setup = { fit <- lm(y ~ x + g, model_1e3); gfit <- glm(y > 3 ~ x + g, binomial, model_1e3) },
      lm_1e3 = vcov(fit), glm_1e3 = vcov(gfit))
bench(".vcov.aliased", setup = { aliased <- c(a = FALSE, b = TRUE, c = FALSE); vc <- diag(2) },
      small = .vcov.aliased(aliased, vc))
bench("weighted.mean", dbl_1e3 = weighted.mean(dbl_1e3, pos_1e3), dbl_1e6 = weighted.mean(dbl_1e6, pos_1e6),
      na_1e3 = weighted.mean(na_1e3, pos_1e3, na.rm = TRUE))
bench("weighted.residuals", setup = { fit <- lm(y ~ x + g, model_1e3, weights = x) },
      lm_1e3 = weighted.residuals(fit))
bench("weights", setup = { fit <- lm(y ~ x + g, model_1e3, weights = x) }, lm_1e3 = weights(fit))
bench("wilcox.test", dbl_1e3 = wilcox.test(dbl_1e3), two_1e3 = wilcox.test(dbl_1e3, pos_1e3),
      small = wilcox.test(dbl_1e3[1:30], pos_1e3[1:30]))
bench("window", ts_1e3 = window(ts_1e3, start = c(2000, 1), end = c(2010, 12)))
bench("window<-", setup = { y <- ts_1e3; value <- dbl_1e3[1:12] }, ts_1e3 = window(y, start = c(2000, 1), end = c(2000, 12)) <- value)
bench("write.ftable", setup = { table <- ftable(xtabs(~ g + b, df_1e3)) }, small = write.ftable(table, file = ""))
bench("xtabs", df_1e3 = xtabs(~ g + b, df_1e3), sum_1e3 = xtabs(x ~ g, df_1e3), df_1e6 = xtabs(~ g + b, df_1e6))
