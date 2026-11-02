x <- as.POSIXlt("2020-01-01", tz = "UTC"); x$year <- integer(0)
try(format(x)); try(as.POSIXct(x)); try(as.Date(x)); try(print(length(x))); try(balancePOSIXlt(x))
d0 <- strptime(as.Date(logical(0)), format="%Y-%m-%d", tz = "GMT"); d0$mday <- 1; try(format(d0)); try(as.POSIXct(d0))
y <- as.POSIXlt(c("2020-01-01", "2020-02-01"), tz = "UTC"); y$mon <- 1L; print(format(y))
for (k in 1:11) { x <- unclass(as.POSIXlt("2020-01-01 10:00", tz = "EST5EDT")); x[[k]] <- x[[k]][0]; class(x) <- c("POSIXlt","POSIXt"); r <- try(format(x), silent=TRUE); cat(k, if (inherits(r, "try-error")) conditionMessage(attr(r,"condition")) else r, "\n") }
for (k in c(1, 7, 8, 9)) { x <- unclass(as.POSIXlt("2020-01-01 10:00", tz = "UTC")); x[[k]] <- x[[k]][0]; class(x) <- c("POSIXlt","POSIXt"); r <- try(as.POSIXct(x), silent=TRUE); cat(k, if (inherits(r, "try-error")) conditionMessage(attr(r,"condition")) else format(r), "\n") }
tmp <- tempfile(); cat("1\n", file = tmp)
n <- nrow(showConnections())
con <- try(file(tmp, open="r", encoding="unknown"))
con <- try(file(tmp, open="r", encoding="bogus-enc"))
print(n == nrow(showConnections()))
