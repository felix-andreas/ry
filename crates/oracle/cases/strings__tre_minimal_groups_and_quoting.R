# With capture groups, a lazy quantifier takes the leftmost-first match as TRE does; without
# groups the shortest overall match wins.
print(regmatches("xaaab", regexpr("x(.+?)(a+)", "xaaab")))
print(regmatches("xaaab", regexpr("x.+?a+", "xaaab")))
print(sub("^(-+BEGIN.+?-+)(.*)$", "\\1", "-----BEGIN KEY-----abc"))
# \Q...\E quotes a literal run.
print(grepl("\\Qa.b\\E", c("a.b", "axb")))
print(gsub("\\Q(*)\\E", "!", "x(*)y(*)"))
