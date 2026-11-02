# POSIX (TRE) matching picks the longest of the leftmost matches.
regexpr("a|ab", "xab")
regmatches("abcd", regexpr("(a|ab)(c|bcd)", "abcd"))
gsub("a|ab", "<\\0>", "abab cab")
sub("(a|ab)(c|bcd)", "[\\1|\\2]", "abcd")
gregexpr("x*", "axxb")[[1]]
gsub("b*", "-", "abc")
strsplit("a1b22c", "[0-9]+")
regexpr("A", "xa", ignore.case = TRUE)

# Nested repetition of variable-width groups runs in linear time, as TRE's automaton does.
s <- paste(c("INFO A", "INFO B", "FATAL x", rep("INFO z", 10), rep("INFO y", 3)), collapse = "#")
grepl(".*A#.*B#.*x(.*z.*){10}(.*y.*){3}", s)
regexpr("(.*z.*){10}", s)

# PCRE backreferences by name, number and relative number.
gsub("(?<w>o)\\g{w}", "0", "foo boo", perl = TRUE)
gsub("(a)\\g1", "X", "aab", perl = TRUE)
gsub("(a)(b)\\g{-1}", "X", "abbc", perl = TRUE)
grepl("(?<q>['\"]).*\\k{q}", c("'x'", "\"y'"), perl = TRUE)
