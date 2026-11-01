# E and O modifiers apply only to the conversions POSIX allows them on; elsewhere they fail.
strptime("2020-01-02", "%EY-%m-%d", tz = "UTC")
strptime("2020-01-02 03", "%Y-%Om-%d %OH", tz = "UTC")
strptime("15:59:59", "%OH:%OM:%OS", tz = "UTC")$hour
strptime("2020-01-02", "%Y-%Om-%d", tz = "UTC")
strptime("Jan 2020", "%Ob %Y", tz = "UTC")
strptime("3 PM", "%I %Op", tz = "UTC")
