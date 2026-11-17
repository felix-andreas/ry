# Sorting and ranking enough strings that the collation runs through per-string sort keys and
# a radix sort over them: few distinct values among many (ranked by counting), many distinct
# values, prefixes longer than eight key bytes, non-ASCII strings among ASCII ones, collator
# attributes, and ties in each ties method.
set.seed(7)
word <- function(n, alphabet, lengths = 1:6) {
    vapply(seq_len(n), function(i) paste(sample(alphabet, sample(lengths, 1), TRUE), collapse = ""), "")
}
digest <- function(x) c(length(x), sum(as.numeric(x) * seq_along(x), na.rm = TRUE) %% 1e9)
mixed <- c(letters[1:4], LETTERS[1:3], "é", "É", "ß", "ø", "-", " ", "1")
pool <- word(40, mixed)
dup <- sample(pool, 3000, TRUE)
for (ties in c("average", "first", "last", "random", "max", "min")) {
    set.seed(1)
    print(digest(rank(dup, ties.method = ties)))
}
print(digest(order(dup)))
print(digest(xtfrm(dup)))
print(head(sort(unique(dup)), 12))
wide <- word(3000, mixed, 1:12)
print(digest(rank(wide)))
print(digest(order(wide, decreasing = TRUE)))
print(identical(sort(wide), wide[order(wide)]))
prefix <- paste0("common-prefix-é-", sample(c("b", "a", "B", "á", "A"), 600, TRUE), sample(0:9, 600, TRUE))
print(digest(rank(prefix, ties.method = "min")))
print(head(unique(sort(prefix)), 15))
na <- dup[1:500]
na[c(3, 50, 499)] <- NA
print(digest(rank(na, na.last = "keep")))
print(sum(is.na(rank(na, na.last = "keep"))))
print(digest(order(na, na.last = FALSE)))
nfd <- rep(c("é", "é", "É", "É", "e", "f"), 100)
print(table(rank(nfd, ties.method = "min")))
print(digest(order(nfd)))
icuSetCollate(alternate_handling = "shifted")
print(digest(rank(dup)))
icuSetCollate(case_first = "upper")
print(head(sort(unique(dup)), 12))
icuSetCollate(locale = "default")
print(digest(rank(dup)))
invisible(NULL)
