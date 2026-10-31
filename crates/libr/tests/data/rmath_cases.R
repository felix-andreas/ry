# Records GNU R's answers for ry's Rmath conformance test (crates/libr/tests/test_rmath.rs).
#
#   Rscript crates/libr/tests/data/rmath_cases.R [output directory]
#
# Builds tests/packages/rmathtest against GNU R, evaluates every case through its C entry
# points, and writes rmath_expected.tsv (function, six arguments, result) and
# rmath_streams.tsv (generator, seed, arguments, draws). Numbers are printed with 17
# significant digits, which round-trips every double; NA and NaN are told apart.

args <- commandArgs(trailingOnly = TRUE)
script <- normalizePath(sub("^--file=", "", grep("^--file=", commandArgs(FALSE), value = TRUE)))
here <- dirname(script)
out_dir <- if (length(args)) args[[1]] else here
source_file <- file.path(here, "..", "packages", "rmathtest", "src", "rmathtest.c")

build <- tempfile("rmathtest")
dir.create(build)
invisible(file.copy(source_file, build))
old <- setwd(build)
status <- system2(file.path(R.home("bin"), "R"), c("CMD", "SHLIB", "-o", "rmathtest.so", "rmathtest.c"),
                  stdout = FALSE, stderr = FALSE)
setwd(old)
if (status != 0) stop("R CMD SHLIB failed")
dyn.load(file.path(build, "rmathtest.so"))

cases <- new.env()
cases$names <- character()
cases$args <- list()

# The function name is a formal with a leading dot, so that no case argument (n, p, ...)
# can partially match it.
add <- function(.name, ...) {
    grid <- expand.grid(..., KEEP.OUT.ATTRS = FALSE, stringsAsFactors = FALSE)
    m <- do.call(cbind, lapply(grid, as.double))
    if (ncol(m) < 6) m <- cbind(m, matrix(0, nrow(m), 6 - ncol(m)))
    cases$names <- c(cases$names, rep(.name, nrow(m)))
    cases$args[[length(cases$args) + 1]] <- m
}

flags <- c(0, 1)
probs <- c(0, 1e-300, 1e-20, 1e-8, 0.001, 0.025, 0.1, 0.3, 0.5, 0.7, 0.9, 0.975, 0.999, 1 - 1e-10, 1)
log_probs <- c(-Inf, -1e5, -1000, -50, -10, -2, -0.5, -0.1, -1e-5, -1e-12, 0)

# p and q over both tails and scales: q functions get probabilities matching the scale.
add_pq <- function(.p_name, .q_name, .xs, ...) {
    params <- list(...)
    do.call(add, c(list(.p_name, x = .xs), params, list(lower = flags, log = flags)))
    do.call(add, c(list(.q_name, p = probs), params, list(lower = flags, log = 0)))
    do.call(add, c(list(.q_name, p = log_probs), params, list(lower = flags, log = 1)))
}

special_x <- c(NA, NaN, -Inf, Inf)

# Special functions.
gx <- c(special_x, -170.5, -10.5, -2.5, -1, -0.5, 0, 1e-300, 1e-8, 0.1, 0.5, 1, 1.5, 2, 2.5, 3,
        7.25, 10, 10.5, 15, 20.3, 50, 100, 171.5, 172, 1e4, 1e10, 1e300)
for (f in c("gammafn", "lgammafn", "digamma", "trigamma", "tetragamma", "pentagamma"))
    add(f, x = gx)
add("lgammafn_sign.sign", x = c(-2.5, -1.5, -0.5, 0.5, 3))
add("lgamma1p", a = c(NaN, -0.49, -0.1, -1e-10, 0, 1e-12, 0.01, 0.2, 0.49, 0.6, 2, 10))
add("log1pmx", x = c(NA, -0.999, -0.8, -0.5, -0.1, -1e-3, -1e-9, 0, 1e-9, 1e-3, 0.009, 0.5, 1, 2, 1e10))
add("log1pexp", x = c(NA, -800, -40, -1, 0, 1, 17, 18.5, 30, 34, 1e10))
add("log1mexp", x = c(NA, 0, 1e-20, 1e-5, 0.5, 0.7, 1, 10, 50, Inf))
add("sign", x = c(NA, NaN, -3, -0, 0, 2, Inf))
add("ftrunc", x = c(NA, -2.7, -0.5, 0.5, 2.7, 1e300, Inf))
for (f in c("cospi", "sinpi", "Rtanpi"))
    add(f, x = c(NA, NaN, Inf, -2.75, -1.5, -1, -0.5, -0.25, 0, 0.1, 0.25, 0.5, 0.75, 1, 1.25, 2.5, 1e15 + 0.5))
add("beta", a = c(NaN, -1, 0, 1e-310, 0.5, 1, 3.5, 100, Inf), b = c(0.5, 2, 70, 180, 1e10))
add("lbeta", a = c(NaN, -1, 0, 1e-310, 0.5, 1, 3.5, 12, 100, 1e5, Inf), b = c(0.5, 2, 9, 70, 180, 1e10))
add("choose", n = c(NA, -5.5, -3, 0, 0.5, 4, 10, 10.5, 50, 100, 1e5), k = c(-1, 0, 1, 2, 3, 5, 29, 30, 31, 45, 60))
add("lchoose", n = c(NA, -5.5, -3, 0, 0.5, 4, 10, 10.5, 50, 100, 1e5), k = c(-1, 0, 1, 2, 3, 5, 29, 30, 31, 45, 60))
add("psigamma", x = c(-2.5, -0.5, 0.3, 1, 2.5, 10, 30.5, 1000), deriv = c(0, 1, 2, 3, 4, 7, 20))
add("dpsifn", x = c(0.3, 1, 2.5, 25), n = c(0, 1, 2, 5), kode = c(1, 2))
add("fmax2", x = c(NA, NaN, -1, 2), y = c(NA, NaN, 1, Inf))
add("fmin2", x = c(NA, NaN, -1, 2), y = c(NA, NaN, 1, -Inf))
add("fsign", x = c(NA, -3, 3), y = c(NaN, -2, -0, 0, 5))
add("fprec", x = c(NA, -123.456789, 0, 1e-320, 0.000123456, 3.14159265358979, 1234567.891, 1e300, Inf),
    digits = c(NaN, -2, 0, 1, 3, 7, 15, 22, 23, Inf))
add("fround", x = c(NA, -2.5, -0.15, 0, 0.15, 0.5, 1.005, 2.675, 1234.5678, 5e-324, 1e15 + 0.3, 1e300, Inf),
    digits = c(NaN, -3, -1, 0, 1, 2, 3, 10, 400, -400))
add("logspace_add", x = c(-Inf, -800, -1, 0, 3), y = c(-Inf, -799, 0, 50))
add("logspace_sub", x = c(0, 3, 750), y = c(-Inf, -800, -1, -1e-20))
add("logspace_sum", n = c(0, 1, 2, 3), a = c(-1, 700), b = c(0, -Inf), c = c(2, -50))
add("R_pow", x = c(NA, NaN, -Inf, -8, -1, -0.5, 0, 0.5, 1, 2, 10, Inf), y = c(NA, -Inf, -3, -0.5, 0, 1 / 3, 2, 3, 0.5, Inf))
add("R_pow_di", x = c(NaN, -2, 0, 0.5, 3, 1.1, Inf), n = c(-1074, -3, -1, 0, 1, 2, 7, 1023))
# pnorm_both leaves the tail it was not asked for unset.
add("pnorm_both.cum", x = c(NaN, -40, -10, -1, 0, 0.5, 3, 9, 40), tail = c(0, 2), log = flags)
add("pnorm_both.ccum", x = c(NaN, -40, -10, -1, 0, 0.5, 3, 9, 40), tail = c(1, 2), log = flags)
add("imax2", x = c(-3, 7), y = c(2, 9))
add("imin2", x = c(-3, 7), y = c(2, 9))

bx <- c(NaN, -1, 0, 1e-10, 0.1, 1, 1.9, 2, 2.5, 10, 30, 100, 700, 750, 1e4)
nus <- c(NaN, -2.5, -2, -1.3, -0.5, 0, 0.3, 0.5, 1, 2.7, 10, 50.5, 120)
add("bessel_j", x = bx, nu = nus)
add("bessel_y", x = bx, nu = nus)
add("bessel_i", x = bx, nu = nus, expo = c(1, 2))
add("bessel_k", x = bx, nu = nus, expo = c(1, 2))

# Continuous distributions.
nx <- c(special_x, -40, -10, -6, -1.5, -0.3, 0, 1e-10, 0.5, 1, 2.2, 5.5, 9, 20, 38, 1e170, 1e200)
add("dnorm", x = nx, mu = c(0, 1, NaN), sigma = c(-1, 0, 1, 2.5, Inf), log = flags)
add_pq("pnorm", "qnorm", nx, mu = c(0, 1.5), sigma = c(0, 1, 3))
add("pnorm", x = c(0, 1), mu = 0, sigma = c(-1, NaN), lower = 1, log = 0)
ux <- c(special_x, -1, 0, 0.25, 1, 2)
add("dunif", x = ux, a = c(0, -1), b = c(0, 1, 3), log = flags)
add_pq("punif", "qunif", ux, a = c(0, -1), b = c(1, 3))
gamx <- c(special_x, -1, 0, 1e-300, 1e-10, 0.01, 0.5, 1, 2.5, 7, 15, 49.5, 100, 1e3, 1e5, 1e10)
shapes <- c(0, 1e-12, 0.01, 0.5, 1, 1.5, 3, 7.5, 20, 99.5, 1000, 1e5)
add("dgamma", x = gamx, shape = shapes, scale = c(0.5, 1), log = flags)
add_pq("pgamma", "qgamma", gamx, shape = shapes, scale = c(1, 2.5))
add("dchisq", x = gamx, df = c(0, 0.3, 1, 2, 3, 10, 150), log = flags)
add_pq("pchisq", "qchisq", gamx, df = c(0.3, 1, 2, 3, 10, 150, 1e4))
add("dexp", x = gamx, scale = c(-1, 0, 0.5, 2), log = flags)
add_pq("pexp", "qexp", gamx, scale = c(0.5, 2))
add("dweibull", x = gamx, shape = c(0.5, 1, 3.2), scale = c(0.7, 2), log = flags)
add_pq("pweibull", "qweibull", gamx, shape = c(0.5, 1, 3.2), scale = c(0.7, 2))
add("dlnorm", x = gamx, meanlog = c(0, -1, 2), sdlog = c(0, 0.3, 1), log = flags)
add_pq("plnorm", "qlnorm", gamx, meanlog = c(0, -1, 2), sdlog = c(0.3, 1))
cx <- c(special_x, -1e300, -1e10, -50, -2, -1, -0.3, 0, 0.7, 1, 3, 1e8, 1e300)
add("dcauchy", x = cx, location = c(0, 2), scale = c(0, 0.5, 3), log = flags)
add_pq("pcauchy", "qcauchy", cx, location = c(0, 2), scale = c(0.5, 3))
add("dlogis", x = cx, location = c(0, 2), scale = c(0, 0.5, 3), log = flags)
add_pq("plogis", "qlogis", cx, location = c(0, 2), scale = c(0.5, 3))
betax <- c(special_x, -0.1, 0, 1e-300, 1e-20, 1e-5, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 1 - 1e-8, 1, 1.5)
betap <- c(0, 1e-8, 0.05, 0.5, 1, 2, 5.5, 14, 60, 450, 1e4, 1e6, Inf)
add("dbeta", x = betax, a = betap, b = c(0.5, 1, 3, 75, 1e5), log = flags)
add("pbeta", x = betax, a = betap, b = c(1e-8, 0.5, 1, 3, 25, 75, 1e5), lower = flags, log = flags)
add("qbeta", p = probs, a = c(1e-3, 0.5, 1, 2, 5.5, 60, 1e4), b = c(0.5, 1, 3, 75), lower = flags, log = 0)
# With one shape 1e5 and p = 1e-300, pbeta's power series is itself noisy at 1e-5 (in GNU R
# as in ry), so the quantile is not determined to more digits; that p is left out there.
add("qbeta", p = probs[-2], a = c(1e-3, 0.5, 1, 2, 5.5, 60, 1e4), b = 1e5, lower = flags, log = 0)
add("qbeta", p = log_probs, a = c(0.5, 2, 5.5, 60), b = c(0.5, 3, 75), lower = flags, log = 1)
tx <- c(special_x, -1e60, -1e10, -50, -5, -1.5, -0.1, 0, 1e-8, 0.4, 2, 8, 40, 1e5, 1e60)
tdf <- c(-1, 0.1, 0.5, 1, 1.5, 2, 3, 7.3, 30, 1e6, 1e25, Inf)
add("dt", x = tx, df = tdf, log = flags)
add_pq("pt", "qt", tx, df = tdf)
fx <- c(special_x, -1, 0, 1e-10, 0.05, 0.5, 1, 2.3, 10, 1e4, 1e10)
fdf <- c(0.5, 1, 2, 3, 12.5, 300, 1e6, Inf)
add("df", x = fx, m = fdf, n = c(0.5, 2, 9, 1e5, Inf), log = flags)
add_pq("pf", "qf", fx, m = c(0.5, 1, 2, 12.5, 300, 1e6, Inf), n = c(0.5, 2, 9, Inf))
add("pf", x = fx, m = c(0.5, 1, 2, 12.5, 300, 1e6, Inf), n = 1e5, lower = flags, log = flags)
add("qf", p = probs[-2], m = c(0.5, 1, 2, 12.5, 300, 1e6, Inf), n = 1e5, lower = flags, log = 0)
# Further out GNU R loses the tail (qf(-1e5, 12.5, 1e5, FALSE, TRUE) is Inf, while the
# quantile is about 5e4).
add("qf", p = log_probs[log_probs > -100], m = c(0.5, 1, 2, 12.5, 300, 1e6, Inf), n = 1e5,
    lower = flags, log = 1)

# Discrete distributions.
kx <- c(special_x, -1, 0, 1, 2, 2.5, 3, 7, 10, 25, 99, 100, 101, 1e4)
add("dbinom", x = kx, n = c(-1, 0, 1, 10, 100, 1e4, 2.5), p = c(-0.1, 0, 1e-10, 0.3, 0.5, 0.97, 1), log = flags)
add_pq("pbinom", "qbinom", kx, n = c(0, 1, 10, 100, 1e4, 1e7), p = c(0, 1e-10, 0.3, 0.5, 0.97, 1))
add("dbinom_raw", x = c(0, 1, 5, 10), n = c(0, 10, 300), p = c(0, 0.05, 0.4), q = c(1, 0.95, 0.6), log = flags)
add("dpois", x = kx, lambda = c(-1, 0, 1e-10, 0.5, 3, 9.5, 10, 50, 1e4, Inf), log = flags)
add("dpois_raw", x = c(0, 0.5, 3, 7.25, 1e5), lambda = c(0, 1e-300, 2.5, 1e5, Inf), log = flags)
add_pq("ppois", "qpois", kx, lambda = c(0, 1e-10, 0.5, 3, 9.5, 10, 50, 1e4, 1e7))
add("dgeom", x = kx, p = c(0, 1e-10, 0.2, 0.9, 1, 1.5), log = flags)
add_pq("pgeom", "qgeom", kx, p = c(1e-10, 0.2, 0.9, 1))
add("dnbinom", x = kx, size = c(0, 1e-12, 0.5, 3, 40, 1e12), prob = c(0, 0.01, 0.3, 0.9, 1), log = flags)
# An infinite size at x = 0 gives answers no consistent rule explains (1, but NaN on the log
# scale, for prob = 1), so only x > 0 is recorded there.
add("dnbinom", x = c(1, 2, 25), size = Inf, prob = c(0.01, 0.3, 0.9, 1), log = flags)
add_pq("pnbinom", "qnbinom", kx, size = c(0, 0.5, 3, 40, 1e10), prob = c(0.01, 0.3, 0.9, 1))
add("dnbinom_mu", x = kx, size = c(0, 1e-12, 0.5, 3, 40, 1e12, Inf), mu = c(0, 0.01, 2.5, 90), log = flags)
add_pq("pnbinom_mu", "qnbinom_mu", kx, size = c(0, 0.5, 3, 40, 1e10, Inf), mu = c(0, 0.01, 2.5, 90))
hx <- c(NaN, -1, 0, 1, 2, 3.5, 5, 10, 20)
add("dhyper", x = hx, r = c(0, 5, 10, 400), b = c(3, 10, 2000), n = c(0, 2, 8, 12), log = flags)
add("phyper", x = hx, r = c(0, 5, 10, 400), b = c(3, 10, 2000), n = c(0, 2, 8, 12), lower = flags, log = flags)
add("qhyper", p = probs, r = c(5, 10, 400), b = c(3, 10, 2000), n = c(2, 8, 12), lower = flags, log = 0)
add("qhyper", p = log_probs, r = c(5, 400), b = c(3, 2000), n = c(2, 12), lower = flags, log = 1)

# Non-central distributions, the studentized range and rank statistics.
add("dnchisq", x = c(NaN, -1, 0, 0.5, 3, 15, 90, 400), df = c(0, 1, 2.5, 10), ncp = c(0, 0.5, 5, 85, 2000), log = flags)
add("pnchisq", x = c(NaN, -1, 0, 0.5, 3, 15, 90, 400, Inf), df = c(0, 1, 2.5, 10), ncp = c(0, 0.5, 5, 85, 2000),
    lower = flags, log = flags)
add("qnchisq", p = c(0, 1e-6, 0.05, 0.5, 0.95, 1), df = c(1, 2.5, 10), ncp = c(0, 0.5, 5, 85), lower = flags, log = 0)
add("qnchisq", p = c(-30, -1, -0.01), df = c(1, 10), ncp = c(0.5, 85), lower = flags, log = 1)
add("dnbeta", x = c(NaN, -0.1, 0, 0.01, 0.3, 0.6, 0.99, 1), a = c(0.5, 2, 10), b = c(0.5, 3), ncp = c(0, 1, 20, 300), log = flags)
add("pnbeta", x = c(NaN, -0.1, 0, 0.01, 0.3, 0.6, 0.99, 1), a = c(0.5, 2, 10), b = c(0.5, 3), ncp = c(0, 1, 20, 300),
    lower = flags, log = flags)
add("qnbeta", p = c(0, 1e-6, 0.05, 0.5, 0.95, 1), a = c(0.5, 10), b = c(0.5, 3), ncp = c(1, 20), lower = flags, log = 0)
add("dnf", x = c(NaN, -1, 0, 0.2, 1, 4, 30), m = c(1, 3, 20), n = c(2, 15, 1e9, Inf), ncp = c(0, 2, 40), log = flags)
add("pnf", x = c(NaN, -1, 0, 0.2, 1, 4, 30), m = c(1, 3, 20), n = c(2, 15, 1e9), ncp = c(0, 2, 40), lower = flags, log = flags)
add("qnf", p = c(0, 0.05, 0.5, 0.95, 1), m = c(1, 20), n = c(2, 15, 1e9), ncp = c(0, 2, 40), lower = flags, log = 0)
add("dnt", x = c(NaN, -Inf, -8, -1, 0, 1e-9, 0.5, 3, 30), df = c(1, 4.5, 30, 1e9), ncp = c(0, -1, 0.5, 5, 39), log = flags)
add("pnt", x = c(NaN, -Inf, -8, -1, 0, 0.5, 3, 30, Inf), df = c(0.5, 1, 4.5, 30, 5e5), ncp = c(0, -1, 0.5, 5, 39, 45),
    lower = flags, log = flags)
add("qnt", p = c(0, 0.01, 0.3, 0.5, 0.9, 1), df = c(0.5, 4.5, 30), ncp = c(0, -1, 2.5), lower = flags, log = 0)
add("ptukey", q = c(NaN, -1, 0, 0.5, 2, 3.5, 6, 20, Inf), rr = c(1, 3), cc = c(2, 5, 20), df = c(2, 10, 150, 1000, 1e5),
    lower = flags, log = flags)
add("qtukey", p = c(0, 0.05, 0.5, 0.95, 0.999, 1), rr = c(1, 2), cc = c(2, 5, 20), df = c(5, 120, 1e5), lower = flags, log = 0)
add("dwilcox", x = c(NaN, -1, 0, 1, 2.5, 7, 12, 30, 50, 51), m = c(0, 1, 4, 10), n = c(1, 3, 5), log = flags)
add("pwilcox", q = c(NaN, -1, 0, 1, 2.5, 7, 12, 30, 50, 51), m = c(1, 4, 10, 25), n = c(1, 3, 5, 30), lower = flags, log = flags)
add("qwilcox", p = probs, m = c(1, 4, 10, 25), n = c(1, 5, 30), lower = flags, log = 0)
add("qwilcox", p = log_probs, m = c(4, 10), n = c(5, 30), lower = flags, log = 1)
add("dsignrank", x = c(NaN, -1, 0, 1, 2.5, 7, 28, 50, 400), n = c(0, 1, 4, 7, 30), log = flags)
add("psignrank", x = c(NaN, -1, 0, 1, 2.5, 7, 28, 50, 400), n = c(1, 4, 7, 30, 60), lower = flags, log = flags)
add("qsignrank", p = probs, n = c(1, 4, 7, 30, 60), lower = flags, log = 0)
add("qsignrank", p = log_probs, n = c(7, 30), lower = flags, log = 1)

fmt <- function(x) {
    out <- sprintf("%.17g", x)
    out[is.na(x) & !is.nan(x)] <- "NA"
    out[is.nan(x)] <- "NaN"
    out
}

args_matrix <- do.call(rbind, cases$args)
result <- suppressWarnings(.Call("rmath_eval", cases$names, as.vector(t(args_matrix))))
stopifnot(ncol(args_matrix) == 6)
lines <- paste(cases$names, apply(matrix(fmt(args_matrix), ncol = 6), 1, paste, collapse = "\t"), fmt(result), sep = "\t")
writeLines(c(paste0("# GNU R ", R.version$major, ".", R.version$minor, "; generated by rmath_cases.R; do not edit"),
             lines), file.path(out_dir, "rmath_expected.tsv"))

streams <- list(
    list("unif_rand", numeric()), list("norm_rand", numeric()), list("exp_rand", numeric()),
    list("rnorm", c(1, 2)), list("rnorm", c(0, 0)), list("runif", c(-1, 3)),
    list("rexp", 2), list("rgamma", c(0.3, 1)), list("rgamma", c(1, 2)), list("rgamma", c(2.5, 1)),
    list("rgamma", c(8, 0.5)), list("rgamma", c(40, 1)), list("rbeta", c(0.5, 0.5)),
    list("rbeta", c(0.2, 3)), list("rbeta", c(2, 3)), list("rbeta", c(30, 7.5)),
    list("rchisq", 3), list("rchisq", 0.6), list("rt", 4), list("rt", 0.8), list("rf", c(3, 9)),
    list("rcauchy", c(0, 1)), list("rlogis", c(1, 2)), list("rweibull", c(1.7, 3)),
    list("rlnorm", c(0, 1)), list("rpois", 0.3), list("rpois", 4), list("rpois", 9.99),
    list("rpois", 10), list("rpois", 75), list("rpois", 1e6), list("rbinom", c(10, 0.3)),
    list("rbinom", c(1000, 0.2)), list("rbinom", c(500, 0.9)), list("rbinom", c(3e5, 0.5)),
    list("rgeom", 0.2), list("rnbinom", c(3, 0.4)), list("rnbinom_mu", c(2, 10)),
    list("rhyper", c(5, 10, 3)), list("rhyper", c(200, 300, 150)), list("rhyper", c(20, 3000, 600)),
    list("rnchisq", c(3, 2)), list("rsignrank", 10), list("rwilcox", c(6, 8)),
    list("rmultinom", c(30, 0.2, 0.5, 0.3)), list("rmultinom", c(1000, 0.1, 0, 0.6, 0.3))
)
stream_lines <- character()
for (seed in c(1, 42)) {
    for (s in streams) {
        set.seed(seed)
        draws <- .Call("rmath_random", s[[1]], as.double(s[[2]]), 25L)
        stream_lines <- c(stream_lines, paste(s[[1]], seed, paste(fmt(s[[2]]), collapse = ","),
                                              paste(fmt(draws), collapse = ","), sep = "\t"))
    }
}
writeLines(c(paste0("# GNU R ", R.version$major, ".", R.version$minor, "; generated by rmath_cases.R; do not edit"),
             stream_lines), file.path(out_dir, "rmath_streams.tsv"))
cat(length(lines), "cases and", length(stream_lines), "streams\n")
