# Probes behind docs/src/content/docs/development/design/big-levers.md: 55 small constructs timed on
# one engine. Run it once per engine and compare the columns:
#
#   Rscript tools/research/big_levers_probes.R > gnur.tsv
#   ry run tools/research/big_levers_probes.R > ry.tsv
#   RY_JIT=off ry run tools/research/big_levers_probes.R > ry-jit-off.tsv
#
# Each line is the probe's name, the best of three timings in nanoseconds per operation, and a
# checksum that must agree between engines. PROBE_FILTER=text runs only the probes whose name
# contains that text.
#
# Each probe is a function of n that runs n operations; the harness reports ns per operation.
probes <- list()
add <- function(name, n, f) probes[[name]] <<- list(n = n, f = f)

inc <- function(x) x + 1
add("call: f(x), tiny leaf", 3e6, function(n) { s <- 0; for (i in 1:n) s <- inc(s); s })

dflt <- function(x, a = 1, b = a + 1) x + a + b
add("call: named + default args", 2e6, function(n) { s <- 0; for (i in 1:n) s <- dflt(x = s, b = 2); s })

inner <- function(x, y = 1) x + y
outer_dots <- function(...) inner(...)
add("call: forwarding ...", 2e6, function(n) { s <- 0; for (i in 1:n) s <- outer_dots(s, y = 1); s })

miss <- function(x, y) if (missing(y)) x + 1 else x + y
add("call: missing()", 2e6, function(n) { s <- 0; for (i in 1:n) s <- miss(s); s })

fib <- function(k) if (k < 2) k else fib(k - 1) + fib(k - 2)
add("call: recursion fib(24)", 75025 * 1.6, function(n) fib(24))

area <- function(s, ...) UseMethod("area")
area.rect <- function(s, ...) s$w * s$h
area.default <- function(s, ...) 0
add("S3: UseMethod on a classed list", 1e6, function(n) { r <- structure(list(w = 2, h = 3), class = "rect"); s <- 0; for (i in 1:n) s <- s + area(r); s })

add("S3: print-free format() of a Date", 2e5, function(n) { d <- as.Date("2026-01-01"); s <- 0L; for (i in 1:n) s <- s + nchar(format(d + i)); s })

suppressMessages({
  methods::setClass("Rect", representation(w = "numeric", h = "numeric"))
  methods::setGeneric("area4", function(s) standardGeneric("area4"))
  methods::setMethod("area4", "Rect", function(s) s@w * s@h)
})
add("S4: generic call + slot reads", 5e5, function(n) { r <- methods::new("Rect", w = 2, h = 3); s <- 0; for (i in 1:n) s <- s + area4(r); s })
add("S4: new()", 1e5, function(n) { s <- 0; for (i in 1:n) s <- s + methods::new("Rect", w = i, h = 3)@w; s })

make_counter <- function() { self <- new.env(); self$count <- 0; self$add <- function(k) { self$count <- self$count + k; invisible(self) }; self }
add("env object: obj$add(1)", 1e6, function(n) { o <- make_counter(); for (i in 1:n) o$add(1); o$count })

add("tryCatch(expr, error=)", 5e5, function(n) { s <- 0; for (i in 1:n) s <- tryCatch(inc(s), error = function(e) 0); s })
add("tryCatch catching stop()", 1e5, function(n) { s <- 0; for (i in 1:n) s <- s + tryCatch(stop("x"), error = function(e) 1); s })
add("withCallingHandlers + warning()", 1e5, function(n) { s <- 0; for (i in 1:n) withCallingHandlers(warning("w"), warning = function(w) { s <<- s + 1; invokeRestart("muffleWarning") }); s })
onex <- function(x) { on.exit(NULL); x + 1 }
add("on.exit()", 1e6, function(n) { s <- 0; for (i in 1:n) s <- onex(s); s })

add("lapply(seq, closure)", 2e6, function(n) { k <- n / 1000; s <- 0; for (j in 1:k) s <- s + length(lapply(1:1000, function(i) i + j)); s })
add("vapply(list, closure, numeric(1))", 2e6, function(n) { k <- n / 1000; l <- as.list(1:1000); s <- 0; for (j in 1:k) s <- s + sum(vapply(l, function(v) v * 2, numeric(1))); s })
add("Map(f, a, b)", 1e6, function(n) { k <- n / 1000; s <- 0; for (j in 1:k) s <- s + length(Map(function(a, b) a + b, 1:1000, 1:1000)); s })
add("Reduce(`+`, list)", 1e6, function(n) { k <- n / 1000; l <- as.list(1:1000); s <- 0; for (j in 1:k) s <- s + Reduce(function(a, b) a + b, l); s })
add("do.call(f, list)", 1e6, function(n) { s <- 0; for (i in 1:n) s <- do.call(inc, list(s)); s })

marg <- function(x, type = c("linear", "quadratic")) { type <- match.arg(type); if (type == "linear") x + 1 else x * x }
add("match.arg()", 5e5, function(n) { s <- 0; for (i in 1:n) s <- marg(s); s })
chk <- function(x) { stopifnot(is.numeric(x), length(x) == 1L); x + 1 }
add("stopifnot(2 conditions)", 5e5, function(n) { s <- 0; for (i in 1:n) s <- chk(s); s })
add("inherits() + is.null() + identical()", 2e6, function(n) { r <- structure(list(), class = c("a", "b")); s <- 0; for (i in 1:n) if (inherits(r, "b") && !is.null(r) && identical(i, i)) s <- s + 1; s })

add("structure(list(...), class=)", 1e6, function(n) { s <- 0; for (i in 1:n) s <- s + length(structure(list(a = i, b = 2), class = "rec")); s })
add("record update: r$a <- r$a + 1", 2e6, function(n) { r <- list(a = 0, b = "x", c = 1:3); for (i in 1:n) r$a <- r$a + 1; r$a })
add("list append: l[[length(l)+1]] <- i", 2e5, function(n) { l <- list(); for (i in 1:n) l[[length(l) + 1L]] <- i; length(l) })
add("vector write: x[i] <- i (prealloc)", 5e6, function(n) { x <- numeric(n); for (i in 1:n) x[i] <- i; x[n] })
add("named lookup: v[['k']] in char-named vec", 2e6, function(n) { v <- c(a = 1, b = 2, k = 3); s <- 0; for (i in 1:n) s <- s + v[["k"]]; s })
add("environment get/assign by string", 1e6, function(n) { e <- new.env(); assign("k", 0, envir = e); for (i in 1:n) assign("k", get("k", envir = e) + 1, envir = e); get("k", envir = e) })

add("paste0('id', i)", 1e6, function(n) { s <- 0L; for (i in 1:n) s <- s + nchar(paste0("id", i)); s })
add("sprintf('%d-%s')", 1e6, function(n) { s <- 0L; for (i in 1:n) s <- s + nchar(sprintf("%d-%s", i, "x")); s })
add("substr + toupper + nchar", 1e6, function(n) { w <- "benchmark"; s <- 0L; for (i in 1:n) s <- s + nchar(toupper(substr(w, 1L, 1L + i %% 5L))); s })
add("gsub(fixed) on a scalar", 5e5, function(n) { s <- 0L; for (i in 1:n) s <- s + nchar(gsub("a", "bb", "banana", fixed = TRUE)); s })
add("strsplit + vapply(nchar)", 2e5, function(n) { s <- 0L; for (i in 1:n) s <- s + length(strsplit("a,b,c,d,e", ",", fixed = TRUE)[[1L]]); s })

add("data.frame(): build 3 cols x 10 rows", 2e4, function(n) { s <- 0L; for (i in 1:n) s <- s + nrow(data.frame(a = 1:10, b = letters[1:10], c = i)); s })
add("df[df$a > 5, ] row filter", 2e4, function(n) { d <- data.frame(a = 1:10, b = letters[1:10]); s <- 0L; for (i in 1:n) s <- s + nrow(d[d$a > 5L, ]); s })
add("df$col <- value", 2e5, function(n) { d <- data.frame(a = 1:10, b = letters[1:10]); for (i in 1:n) d$c <- i; ncol(d) })
add("rbind(df, df)", 5e3, function(n) { d <- data.frame(a = 1:10, b = letters[1:10]); s <- 0L; for (i in 1:n) s <- s + nrow(rbind(d, d)); s })
add("merge(df, df) 100 rows", 1e3, function(n) { a <- data.frame(k = 1:100, x = runif(100)); b <- data.frame(k = 100:1, y = runif(100)); s <- 0L; for (i in 1:n) s <- s + nrow(merge(a, b, by = "k")); s })
add("aggregate(x ~ g) 1000 rows", 3e2, function(n) { d <- data.frame(g = rep(1:10, 100), x = as.numeric(1:1000)); s <- 0L; for (i in 1:n) s <- s + nrow(aggregate(x ~ g, d, sum)); s })
add("factor() + table() 1000 values", 5e3, function(n) { v <- rep(letters[1:10], 100); s <- 0L; for (i in 1:n) s <- s + length(table(factor(v))); s })
add("tapply(x, g, sum) 1000 values", 2e4, function(n) { g <- rep(1:10, 100); x <- as.numeric(1:1000); s <- 0; for (i in 1:n) s <- s + sum(tapply(x, g, sum)); s })
add("seq_len/rev/sum small vectors", 2e6, function(n) { s <- 0; for (i in 1:n) s <- s + sum(rev(seq_len(8L))); s })
add("ifelse() on length-10 vectors", 5e5, function(n) { x <- c(-2, 3, -1, 5, 0, 2, -7, 1, 4, -3); s <- 0; for (i in 1:n) s <- s + sum(ifelse(x > 0, x, -x)); s })

set.seed(1); X <- runif(1e7); Y <- runif(1e7); G <- sample.int(1000L, 1e7, TRUE); CH <- as.character(G[1:2e6])
add("vec: sum(x*y + x) 1e7", 1e7 * 5, function(n) { s <- 0; for (j in 1:5) s <- s + sum(X * Y + X); s })
add("vec: x[x > 0.5] 1e7", 1e7 * 5, function(n) { s <- 0; for (j in 1:5) s <- s + length(X[X > 0.5]); s })
add("vec: exp(x) + log(y) 1e7", 1e7 * 3, function(n) { s <- 0; for (j in 1:3) s <- s + sum(exp(X) + log(Y)); s })
add("vec: ifelse(x > y, x, y) 1e7", 1e7 * 2, function(n) { s <- 0; for (j in 1:2) s <- s + sum(ifelse(X > Y, X, Y)); s })
add("vec: cumsum + diff 1e7", 1e7 * 3, function(n) { s <- 0; for (j in 1:3) s <- s + sum(diff(cumsum(X))); s })
add("vec: sort(x) 1e7 doubles", 1e7, function(n) sum(sort(X)[1:10]))
add("vec: order(g) 1e7 ints", 1e7, function(n) sum(order(G)[1:10]))
add("vec: rowsum(x, g) 1e7", 1e7 * 2, function(n) { s <- 0; for (j in 1:2) s <- s + sum(rowsum(X, G)); s })
add("vec: match + unique 1e7 ints", 1e7 * 2, function(n) { s <- 0; for (j in 1:2) s <- s + sum(match(G, unique(G))); s })
add("vec: paste0 on 2e6 strings", 2e6, function(n) sum(nchar(paste0(CH, "_x"))))
add("vec: mean/var/sd 1e7", 1e7 * 3, function(n) mean(X) + var(X) + sd(Y))
add("vec: matrix %*% 500x500", 2, function(n) { m <- matrix(X[1:250000], 500); sum(m %*% m) + sum(m %*% m) })

only <- Sys.getenv("PROBE_FILTER", "")
for (name in names(probes)) {
  if (nzchar(only) && !grepl(only, name, fixed = TRUE)) next
  p <- probes[[name]]
  p$f(max(1, p$n %/% 20))
  best <- Inf; value <- NULL
  for (rep in 1:3) { t <- system.time(value <- p$f(p$n))[["elapsed"]]; best <- min(best, t) }
  cat(sprintf("%s\t%.1f\t%.6g\n", name, best / p$n * 1e9, as.numeric(value)[1]))
}
