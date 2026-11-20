# GNU R's own strptime: `%D` reads `%y/%m/%d`, `%s` reads seconds since the epoch and breaks
# them down in `tz` (later fields override), numeric fields and `%z` skip only spaces, and names,
# `%p` and `%%` skip nothing.
print(strptime("01/02/24", "%D", tz = "UTC"))
print(strptime("24/01/02", "%D", tz = "UTC"))
print(strptime("1700000000", "%s", tz = "UTC"))
print(strptime("1700000000", "%s", tz = "America/New_York"))
print(strptime("1700000000.5", "%s", tz = "UTC"))
print(strptime(c(" -86400x", " 1700000000", "+1700000000", "-1"), "%s", tz = "UTC"))
print(strptime("0", "%s", tz = "UTC"))
print(strptime(c("12345678901234567890", "99999999999999999"), "%s", tz = "UTC"))
print(strptime("9999999999999", "%s", tz = "UTC"))
print(strptime("2024-01-02 1700000000", "%Y-%m-%d %s", tz = "UTC"))
print(strptime("1700000000 2020", "%s %Y", tz = "UTC"))
print(strptime("1700000000 05", "%s %H", tz = "UTC"))
print(unlist(unclass(strptime("1700000000", "%s", tz = "Europe/Berlin"))))
print(strptime("2024-\t01-02", "%Y-%m-%d", tz = "UTC"))
print(strptime("2024- 01-02", "%Y-%m-%d", tz = "UTC"))
print(strptime(c(" Jan 2024 1", "Jan 2024 1"), "%b %Y %d", tz = "UTC"))
print(strptime(c("3 PM", "3PM"), "%I%p", tz = "UTC") == strptime("15", "%H", tz = "UTC"))
print(strptime(c("5 %", "5%"), "%d%%", tz = "UTC") == strptime("5", "%d", tz = "UTC"))
print(as.POSIXct(strptime(c("x\t+0100", "x +0100", "x+0100"), "x%z", tz = "UTC")) ==
  as.POSIXct(strptime("00", "%H", tz = "UTC")) - 3600)
# as.Date tries its formats on the first string, which a tab before the digits defeats.
print(try(as.Date("\t2024-01-02"), silent = TRUE))
print(as.Date(c(" 2024-01-02", "2024-01-02 ")))
print(as.Date(c(NA, " 2024/01/02")))
# `tz` must be one string; NA names the zone "NA", which has no abbreviation.
print(try(strptime("2024-01-02", "%Y-%m-%d", tz = NA), silent = TRUE))
print(try(as.POSIXlt("2024-01-02", tz = c("UTC", "GMT")), silent = TRUE))
print(as.POSIXct("2024-01-02", tz = NA_character_))
x <- as.POSIXlt(as.POSIXct(0, tz = "UTC"), tz = NA_character_)
print(x$zone)
print(attr(x, "tzone"))
