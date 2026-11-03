# `%OSn` prints n decimals, truncated, and `%OS0` none; a bare `%OS` follows `digits.secs`.
# Only the first `%OS` is widened: a later one is `%S` followed by a literal digit.
x <- as.POSIXct("2026-10-01 08:00:07.123456", tz = "UTC")
format(as.POSIXct("2026-10-01 08:00:00", tz = "UTC"), "%H:%M:%OS0")
for (f in c("%OS0", "%OS1", "%OS3", "%OS6", "%OS9", "%OS", "%OSx", "%OS0%OS2", "%S %OS2", "%Od %OH"))
  print(format(x, f, tz = "UTC"))
format(as.POSIXct("2026-10-01 08:00:59.9999", tz = "UTC"), c("%OS0", "%OS2"))
options(digits.secs = 3); format(x, "%OS"); format(x, "%OS0"); format(x)
options(digits.secs = NULL)
# Parsing `%OS` reads the longest number, keeps it only when it lies in [0, 61], and treats
# a digit after it as literal text.
p <- function(s, f = "%H:%M:%OS") format(strptime(s, f, tz = "UTC"), "%H:%M:%OS6")
p("08:00:07.5"); p("08:00: 7"); p("08:00:1e1"); p("08:00:07,5"); p("08:00:70")
p("08:00:61"); p("08:00:.5"); p("08:00:-1"); p("08:00:"); p("08:00:7.")
p("08:00:07", "%H:%M:%OS0"); p("08:00:070", "%H:%M:%OS0"); p("08:00:07.123", "%H:%M:%OS3")
p("080007", "%H%M%OS"); p("08:00:07.25 PM", "%I:%M:%OS %p")
format(strptime("2020-01-01 24:00:00.5", "%Y-%m-%d %H:%M:%OS", tz = "UTC"), "%F %H:%M:%OS1")
