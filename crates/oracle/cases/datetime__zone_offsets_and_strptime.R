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

# A month without a day cannot be completed from today's date, and date-time arithmetic
# takes precedence over Ops.difftime.
strptime(c("2020-10", "Oct-2020"), c("%Y-%m", "%b-%Y"), tz = "UTC")
start <- as.POSIXct("2008-08-03", tz = "UTC")
start + 2 * difftime(start + 86400, start)
as.Date("2020-01-01") - as.difftime(3, units = "days")

# A repeated fall-back time takes the offset in force at the wall clock read as UTC.
format(as.POSIXct("2021-10-31 02:30:00", tz = "Europe/Berlin"), "%Z")
format(as.POSIXct("2021-11-07 01:30:00", tz = "America/New_York"), "%Z")
# %Z cannot be parsed, %OSn is output only, and %p adjusts only a 12-hour field.
tryCatch(strptime("2020-01-01 UTC", "%Y-%m-%d %Z"), error = conditionMessage)
strptime("12:00:00.123", "%H:%M:%OS3", tz = "UTC")$sec
strptime("2020-01-01 5 pm", "%Y-%m-%d %H %p", tz = "UTC")

# %x reads year first, hour 24 allows only 24:00:00, and week numbers with a weekday give a date.
print(strptime(c("24/01/15", "01/15/24"), "%x", tz = "UTC"))
print(strptime(c("2024-01-01 24:30", "2024-01-01 24:00"), "%Y-%m-%d %H:%M", tz = "UTC"))
print(strptime("2024 10 1", "%Y %U %w", tz = "UTC"))
print(strptime(c("2024 10 1", "2024 10 7"), "%Y %W %u", tz = "UTC"))
print(strptime("19", "%C", tz = "UTC")$year)
print(strptime("19 05", "%C %y", tz = "UTC")$year)
print(strptime("-100-01-01", "%Y-%m-%d", tz = "UTC"))
print(withCallingHandlers(strptime("2024-01-01 +1500", "%Y-%m-%d %z", tz = "UTC"),
  warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }))
print(strptime(NA, "%Z"))
print(strptime("abc", "%Y %Z"))
print(tryCatch(strptime("x", "%Z"), error = conditionMessage))
