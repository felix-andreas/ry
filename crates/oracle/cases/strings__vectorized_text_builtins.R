# The vectorized string builtins over NA, empty input, non-ASCII, latin1 and bytes-marked
# strings, names and dim, and vectors long enough (64 elements and more) to hold many strings.
latin <- "caf\xe9"
Encoding(latin) <- "latin1"
raw_bytes <- "a\xffb"
Encoding(raw_bytes) <- "bytes"
x <- c(first = "abc", second = NA, third = "", fourth = "été", fifth = latin)
m <- matrix(c("ab", "cd", NA, "über"), 2, dimnames = list(c("r1", "r2"), c("c1", "c2")))
long <- paste0(rep(c("alpha", "Beta", "ça", "", "x_y"), length.out = 70), seq_len(70) %% 7)
long[c(5, 40)] <- NA

print(nzchar(x))
print(nzchar(x, keepNA = TRUE))
print(nzchar(character()))
print(nzchar(m))
print(sum(nzchar(long, keepNA = TRUE), na.rm = TRUE))
print(nzchar(c(1, NA)))

print(strtrim(x, 2))
print(strtrim(m, c(1, 3)))
print(strtrim(character(), integer()))
print(strtrim(c("中文字", "abcdef"), 3))
print(table(nchar(strtrim(long, 3)), useNA = "ifany"))
print(tryCatch(strtrim("a", NA), error = conditionMessage))
print(tryCatch(strtrim(1, 1), error = conditionMessage))

print(toupper(x))
print(tolower(m))
print(toupper(character()))
print(tryCatch(toupper(raw_bytes), error = conditionMessage))
print(tolower(long)[1:12])
print(chartr("abc", "xyz", long)[1:6])

print(startsWith(x, "a"))
print(endsWith(x, "é"))
print(endsWith(c("ab", "b", NA, ""), c("b", NA)))
print(startsWith(character(), "a"))
print(sum(startsWith(long, "al"), na.rm = TRUE))
print(sum(endsWith(long, "3"), na.rm = TRUE))
print(endsWith(latin, "é"))
print(tryCatch(startsWith(1, "a"), error = conditionMessage))

print(make.names(c("a b", "", NA, "1x", ".2", "if", "été", "_u", "ok.name")))
print(make.names(c("a_b", "a-b"), allow_ = FALSE))
print(make.names(character()))
print(length(unique(make.names(long))))

print(abbreviate(c(first = "International Business", second = "abc", third = NA), 5))
print(abbreviate(character(), 3))
print(unname(abbreviate(long[c(1:2, 4:7, 9:12)], 4)))

print(intToUtf8(c(72, 0, 233, NA, 0x4e2d), multiple = TRUE))
print(intToUtf8(c(72, 0, 233, 0x4e2d)))
print(intToUtf8(c(72, NA)))
print(intToUtf8(integer()))
print(intToUtf8(c(0xD83D, 0xDE00), allow_surrogate_pairs = TRUE))
print(nchar(intToUtf8(rep(c(97, 0xe9), 40))))

print(rawToChar(as.raw(c(72, 105, 0, 0))))
print(rawToChar(as.raw(c(72, 0, 105)), multiple = TRUE))
print(rawToChar(raw()))
print(rawToChar(raw(), multiple = TRUE))
print(tryCatch(rawToChar(as.raw(c(72, 0, 105))), error = conditionMessage))
print(nchar(rawToChar(as.raw(rep(65:90, 3)))))

print(pmatch(c("me", "mo", "", NA, "é"), c("mean", "median", "mode", "NA", "été")))
print(pmatch(c("a", "a"), c("ab", "ac", "a"), duplicates.ok = TRUE))
print(pmatch(character(), "a"))
print(pmatch(NULL, c("a", "b")))
print(sum(!is.na(pmatch(long, long))))
print(charmatch(c("me", "mea", "", NA, "zz"), c("mean", "median", "NA")))
print(charmatch(character(), "a"))
print(charmatch(long[1:20], c("alpha1", "Beta2", "ça3"), nomatch = -1L))
print(tryCatch(pmatch(1, "a"), error = conditionMessage))

print(grepl("a", x))
print(grep("é", x, value = TRUE))
print(grep("b", m))
print(grep("^a", long, value = TRUE, invert = TRUE)[1:5])
print(grepl("a", character()))
print(grepl(NA, c("a", NA)))
print(grep(NA, c("a", "b"), value = TRUE))
print(sub("a", "A", x))
print(gsub("(.)", "<\\1>", m))
print(gsub("a", "", long)[1:8])
print(sub("\xff", "!", raw_bytes, useBytes = TRUE))
print(regexpr("t", x))
print(gregexpr("a", long[1:3]))
print(regexec("(a)(l)?", long[1:4]))
