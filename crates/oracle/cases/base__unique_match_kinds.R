# unique, duplicated, anyDuplicated and match over every key kind: dense and sparse integers,
# doubles with NA, NaN and both zeros, ASCII and non-ASCII strings, NA_character_, lists.
ints <- c(3L, NA, 1L, 3L, 2L, NA, 1L, 100000000L, -5L, 100000000L)
for (x in list(ints, c(1, NA, NaN, 0, -0, NA, NaN, Inf, -Inf, 1), c("b", NA, "a", "b", NA, "c"),
               c(TRUE, NA, FALSE, TRUE), as.complex(c(1, NA, 1, 2)), as.raw(c(1, 2, 1)),
               list(1, "a", 1, c(1, 2), "a", NULL, NULL))) {
  str(unique(x)); print(duplicated(x)); print(duplicated(x, fromLast = TRUE))
  print(anyDuplicated(x)); print(anyDuplicated(x, fromLast = TRUE))
  str(unique(x, fromLast = TRUE))
}
print(unique(ints, incomparables = NA)); print(duplicated(ints, incomparables = c(NA, 3L)))
print(anyDuplicated(c(1, 2, NA, NA), incomparables = NA))
print(match(c(2L, NA, 7L, 3L), ints)); print(match(c(NaN, NA, 0, -0), c(1, NA, NaN, 0)))
print(match(c("c", NA, "z"), c("a", NA, "c"))); print(match(1:3, c(3, 2.5, 1)))
print(match(c("1", "2"), 1:3)); print(match(c(1L, NA, 4L), c(4L, NA), nomatch = 0L))
print(match(c("a", "b", NA), c("b", NA), incomparables = NA))
print(c("x", "q") %in% c("q", "r"))
# A bitset over more than one word, and runs around word boundaries.
long <- rep_len(c(1:70, 1:3), 200)
print(anyDuplicated(1:130)); print(anyDuplicated(c(1:130, 64L))); print(which(duplicated(long)))
print(length(unique(long))); print(anyDuplicated(long, fromLast = TRUE))
big <- c("north", "south", "east", "west")[1:100000 %% 4L + 1L]
print(unique(big)); print(table(match(big, c("west", "east")), useNA = "ifany"))
print(levels(factor(big))); print(anyDuplicated(seq_len(100000)))
# Equal text in different encodings matches; NA never equals the string "NA".
latin <- iconv("café", "UTF-8", "latin1")
utf8 <- enc2utf8("café")
print(Encoding(c(latin, utf8))); print(unique(c(latin, utf8, "cafe")))
print(match(utf8, c("x", latin))); print(duplicated(c(latin, utf8)))
print(match(c("NA", NA), c(NA, "NA")))
print(anyDuplicated(c("NA", NA))); print(unique(c(NA, "NA", NA_character_)))
print(unique(factor(c("b", "a", "b")))); print(match(factor("b"), factor(c("a", "b"))))
words <- c(as.character(1:200), "70", "150", as.character(201:300))
reals <- c(as.numeric(1:200) / 4, 70 / 4, 150 / 4, 201:300 / 4)
for (v in list(words, reals)) {
  print(anyDuplicated(v)); print(anyDuplicated(v, fromLast = TRUE)); print(which(duplicated(v)))
  print(which(duplicated(v, fromLast = TRUE))); print(length(unique(v)))
}
