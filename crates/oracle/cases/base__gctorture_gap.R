# gctorture2 collects before every step-th allocation and returns the previous step; a
# negative or missing step leaves the setting alone. gctorture returns whether stress was on.
print(gctorture2(100))
print(gctorture2(NA))
print(gctorture2(-1))
print(gctorture2(5, 2))
print(gctorture(FALSE))
print(gctorture2(0))
print(gctorture(TRUE))
print(gctorture(FALSE))
print(gctorture2(1000)); x <- lapply(1:20000, function(i) list(i)); print(length(x)); print(gctorture2(0))
