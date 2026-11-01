# POSIXlt subsetting through unCfillPOSIXlt and balancePOSIXlt, and date-time names, which live
# on the year component.
x <- as.POSIXlt(c(a = "2020-01-02 03:04:05", b = "2021-06-07 08:09:10"), tz = "UTC")
print(x[1]); print(x[2, "year"]); print(x[, "mon"])
y <- unclass(as.POSIXlt("2020-01-02", tz = "UTC")); y$sec <- c(1, 2, 3)
z <- base:::unCfillPOSIXlt(structure(y, class = c("POSIXlt", "POSIXt")))
print(lengths(z)); print(attr(z, "balanced")); print(class(z))
print(attributes(balancePOSIXlt(as.POSIXlt("2020-01-02", tz = "UTC")))[c("class", "balanced")])
print(format(as.Date("2015-01-01")))
print(format(c(a = as.Date("2015-01-01"))))
