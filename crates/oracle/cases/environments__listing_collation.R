# ls() and sort() order names by the locale's collation: ICU's root order in C.UTF-8
# (case and punctuation as secondary differences) and byte order under C.
names <- c("b", "B", "a_b", "a.b", "ab", "A", "_x", ".y", "Zeta", "zeta", "x1", "x10", "x2",
           "a__b", "..z", "a-b", "Ab", "aB", "AB", "a1", "a_1", "a.1", "abc", "ab_")
e <- new.env()
for (n in names) assign(n, 1, envir = e)
print(ls(e))
print(ls(e, all.names = TRUE))
print(sort(names))
print(names(as.list(e, all.names = TRUE, sorted = TRUE)))
accented <- c("e", "é", "E", "f", "ea", "éa", "Éa")
print(sort(accented))
invisible(Sys.setlocale("LC_COLLATE", "C"))
print(ls(e, all.names = TRUE))
print(sort(names))
print(sort(accented))
