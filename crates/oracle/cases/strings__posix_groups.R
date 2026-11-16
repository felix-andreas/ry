# POSIX (TRE) capture groups: each repetition takes the longest span the whole match allows.
cases <- list(
  c("a*(a*)", "aa"), c("(a*)(a*)", "aaa"), c(".*_(\\d+)", "x_1_23"), c("(.*)_(.*)", "a_b_c"),
  c("x*(x?)y", "xxy"), c("(a+)(a+)", "aaaa"), c("([a-z]*)([a-z]*)z", "abz"), c("a?(ab)?", "ab"),
c("(.?)(.*)", "abc"), c("[0-9]*([0-9])", "123"),
  c("^(a*)(b?)b*$", "aabbb"), c("((a*)b*)(b*)", "aabb"), c("(a|ab)(c|bcd)", "abcd"),
  c("(a*)(ab)*b", "aabab"), c("id([0-9]+)_([a-z])", "xid00042_q"), c("(x*)(y*)", "zzz"),
  c("([[:alpha:]]+)([[:digit:]]{2,3})", "abc12345"), c("(A+)(a*)", "AAaa"))
for (p in cases) {
  m <- regmatches(p[2], regexec(p[1], p[2]))[[1]]
  cat(sprintf("%-34s %-8s %s\n", p[1], p[2], paste(m, collapse = "|")))
}
x <- c("id00042_q", "id7_z", "nope", NA, "id1_a id22_b")
print(gsub("id([0-9]+)_([a-z])", "\\2-\\1", x))
print(sub("(a*)(a*)", "<\\1|\\2>", c("aaa", "baa", "")))
print(gsub("([a-z]*)([0-9]*)", "[\\1,\\2]", "ab12cd3"))
print(regexec("(A+)(a*)", "xAAaa", ignore.case = TRUE))
print(gsub("(.*)\\.(.*)", "\\2", c("file.tar.gz", "noext")))
