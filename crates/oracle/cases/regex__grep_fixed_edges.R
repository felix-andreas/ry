# grep and grepl with fixed = TRUE: short and long subjects, repeated first bytes, NA, an empty
# pattern, invert, value with names, and UTF-8 and Latin-1 text.
x <- c(a = "bond", b = "bbbond", c = NA, d = "", e = strrep("bon", 40), f = paste0(strrep("x", 70), "bond"),
       g = "café bond", h = "bon")
print(grep("bond", x, fixed = TRUE))
print(grepl("bond", x, fixed = TRUE))
print(grep("bond", x, fixed = TRUE, value = TRUE))
print(grep("bond", x, fixed = TRUE, invert = TRUE))
print(grep("bond", x, fixed = TRUE, invert = TRUE, value = TRUE))
print(grepl("", x, fixed = TRUE))
print(grep("é", x, fixed = TRUE))
latin <- iconv("café", "UTF-8", "latin1")
print(Encoding(latin))
print(grepl("é", c(latin, "cafe"), fixed = TRUE))
print(grepl("b", character(), fixed = TRUE))
print(grep("[", c("a[b", "ab"), fixed = TRUE))
