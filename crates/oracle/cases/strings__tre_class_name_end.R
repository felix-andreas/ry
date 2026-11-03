# TRE ends a class name at the first colon and skips one character, so `[[:lower::]]+` reads as
# `[[:lower:]]]+`. GNU R's own install.packages() relies on this pattern.
x <- c("abc", ":", "]", "a]", "A", "mac.binary", "source", ":]")
print(grepl("^[[:lower::]]+", x))
print(regmatches(x, regexpr("[[:lower::]]+", x)))
grepl("^[[:lower::]]+[.]binary", c("source", "mac.binary", "both"))
