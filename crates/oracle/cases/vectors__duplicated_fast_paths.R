# `duplicated` hashes plain keys per type when there are no incomparables, and a character
# vector of ASCII strings by its interned string objects; `strsplit` with a fixed pattern
# searches with a prebuilt finder.
print(unique(c(NA, NaN, 0, -0, 1, NA, NaN)))
print(duplicated(c(1.5, NA, 1.5, NaN, NA, NaN)))
print(duplicated(c("a", NA, "a", "b", NA), fromLast = TRUE))
print(unique(c(TRUE, NA, FALSE, TRUE, NA)))
print(duplicated(c(3L, NA, 3L, 1L, NA), fromLast = TRUE))
print(unique(c("é", iconv("é", "UTF-8", "latin1"), "e", "é")))
print(unique(c(1L, 2L, 1L, 2L), incomparables = 1L))
print(unique(c("a", "b", "a"), incomparables = "a"))
print(unique(list(1, "a", 1, NULL, NULL)))
print(anyDuplicated(c(3L, 1L, 3L)))
print(anyDuplicated(c("x", "y", "z")))
print(strsplit(c("a1b1c", "1", "", NA, "11a", "abc"), "1", fixed = TRUE))
print(strsplit(c("one--two----three", "--"), "--", fixed = TRUE))
print(strsplit(c("abc", "été"), ""))
print(strsplit("a.b.c", ".", fixed = TRUE))
