# Conversions between instants and wall-clock times in named zones, across daylight saving.
ny <- "America/New_York"
x <- as.POSIXct("2013-01-01 00:00:00", tz = "America/Chicago")
unclass(x)
format(x, "%Y-%m-%d %H:%M:%S %z %Z")
format(as.POSIXct("2010-07-05 10:00", tz = "Europe/Paris"), usetz = TRUE)
format(as.POSIXct("2013-01-01", tz = "Asia/Kolkata"), "%z %Z")

# A time in the spring-forward gap, and the two readings of a time the clocks pass twice.
unclass(as.POSIXct("2010-03-14 02:30:00", tz = ny))
unclass(as.POSIXct("2010-11-07 01:30:00", tz = ny))
lt <- as.POSIXlt("2010-11-07 01:30:00", tz = ny)
lt$isdst <- 0L
unclass(as.POSIXct(lt))

# The zone fields of a POSIXlt.
lt <- as.POSIXlt(as.POSIXct(1268548200, tz = ny))
str(unclass(lt)[c("hour", "isdst", "zone", "gmtoff")])
attr(lt, "tzone")
attr(as.POSIXlt(as.POSIXct(0, tz = "UTC"), "Australia/Sydney"), "tzone")
str(unclass(as.POSIXlt(NA_real_, tz = ny, origin = "1970-01-01"))[c("isdst", "zone", "gmtoff")])
str(unclass(as.POSIXlt(NA_real_, tz = "UTC", origin = "1970-01-01"))[c("isdst", "zone", "gmtoff")])

# Wall-clock fields out of range normalize in the zone.
lt <- as.POSIXlt("2010-07-05 10:00", tz = "Europe/Paris")
lt$hour <- 30L
format(lt, "%Y-%m-%d %H %z %Z")
as.POSIXct(lt)

# strptime with zones, offsets and the less common directives.
str(unclass(strptime("2010-01-05 10:00 +0200", "%Y-%m-%d %H:%M %z", tz = ny))[c("hour", "isdst", "zone", "gmtoff")])
str(unclass(strptime("2010-03-14 02:30:00", "%Y-%m-%d %H:%M:%S", tz = ny))[c("hour", "isdst", "zone", "gmtoff")])
strptime("Mon Aug 13 11:05:09 PM 2012", "%a %b %d %I:%M:%S %p %Y", tz = "UTC")
strptime(" 3/ 4/99", "%m/%d/%y", tz = "UTC")
strptime("12:30:15.25", "%H:%M:%OS", tz = "UTC")$sec
strptime("2012-08-13T11:05:09Z", "%Y-%m-%dT%H:%M:%S%z", tz = "UTC")
strptime("2012-02-30", "%Y-%m-%d", tz = "UTC")
format(as.POSIXct(c(0, NA), origin = "1970-01-01", tz = "UTC"), usetz = TRUE)

# The rarer strftime directives.
d <- as.Date(c("2010-01-01", "2012-12-31", "2016-01-03", "2021-01-01"))
format(d, "%G-%V %g %U %W %u %C %s")
format(as.POSIXct("1999-03-04 00:30:00", tz = "UTC"), "%k|%l|%P|%r|%c|%Ey|%Od|%x|%X")
