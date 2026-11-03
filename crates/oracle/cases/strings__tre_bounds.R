outcome <- function(pattern, x, ...) {
  tryCatch(
    paste(grepl(pattern, x, ...), collapse = " "),
    warning = function(w) paste("warning:", conditionMessage(w)),
    error = function(e) paste("error:", conditionMessage(e))
  )
}
show <- function(patterns, x) {
  for (pattern in patterns) cat(sprintf("%-20s", pattern), outcome(pattern, x), "\n")
}

# Approximate bounds: insertions, deletions, substitutions and all edits.
show(c("^a{+}$", "^a{-}$", "^a{#}$", "^a{~}$", "^a{~1}$", "^a{+1}$", "^a{+2}$",
       "^a{+0}$", "^a{0}$", "^a{0+}$", "a{+}", "^a{+}b$", "^a{1i<2}$", "^a{+}?$",
       "^a{0,1}{+}$", "^a{+-}$"),
     c("aaa", "abc", "a", "", "b", "ba", "xa", "ab"))
grepl("^[A-Z]{+}$", "Equity", ignore.case = TRUE)
grepl("A{+}", "xya", ignore.case = TRUE)

# TRE's quirks in plain bounds.
show(c("^a{,1}$", "^a{,2}$", "^a{,3}$", "^a{,}$", "^a{0,}$", "^a{2}{,1}$", "^a{+}??$"),
     c("", "a", "aa", "aaa", "aaaa", "aaaaa"))
show(c("{2}a", "a{0}b", "a{0,0}", "a{255}"), c("aa", "{2}a", "a{2}"))

# Groups and nested scopes.
show(c("^(ab){+}$", "^(ab){-}$", "^(ab){#}$", "^(ab){~1}$", "^(a|b){+1}$"),
     c("ab", "aab", "xab", "axb", "b", "a", "abab", "xaxb"))
show(c("^a{+}b{+}c{+}$", "^(a{+}b){-}$"), c("abcd", "xaybzcwd", "abcdx", "xabcd"))

# Match positions, groups and replacements.
x <- c("xaaay", "xbay", "aXbXc", "abc")
for (pattern in c("a{+}", "b{+}c", "a{-}b", "a{#}c", "a{~1}c", "(a)(b){+}c", "ab{ 2i+1d<3}c"))
  print(regmatches(x, regexpr(pattern, x)))
sub("(a)(b){+}c", "[\\1|\\2]", c("abc", "axbc", "abxc"))
gsub("a{+}", "-", c("baaab", "xyz", ""))
regexpr("a{+}", c("baaab", "xyz"))
gregexpr("b{-}c", "abcxacbc")[[1]]
regexec("(a)(b){+}c", "zaxbc")
strsplit("a1b22c333d", "[0-9]{+}")

# fixed and perl patterns do not read approximate bounds.
grepl("a{+}", c("aaa", "a{+}"), fixed = TRUE)
grepl("^a{+}$", c("aaa", "a{+}"), perl = TRUE)

# Bounds that TRE rejects.
show(c("^a{*}$", "^a{?}$", "^a{}$", "^a{ 1}$", "^a{,0}$", "a{256}", "a{2,1}", "a{2",
       "a{2}*", "a{2}+", "a{+}*", "a{2+}", "a{,2+}", "a{1,2+}", "a{1,2#}"),
     "aa")

# More than three nested approximate scopes trip an assertion in TRE, but only when a match
# reaches them.
tryCatch(grepl("a{+}b{+}c{+}d{+}", "abcd"), error = function(e) conditionMessage(e))
grepl("a{+}b{+}c{+}d{+}", "zzz")
