# perl = TRUE runs on PCRE2, so PCRE-only syntax works: named backreferences, an optional
# lookahead, case conversion in replacements, and Unicode properties.
print(grepl("(?<q>['\"])x\\g{q}", c("'x'", "\"x'"), perl = TRUE))
print(regmatches("2020-01-02T", regexpr("\\d+(?=\\D+)?", "2020-01-02T", perl = TRUE)))
print(gsub("(\\w+)@(\\w+)", "\\2 at \\U\\1", "user@example", perl = TRUE))
print(regexpr("(?<year>\\d{4})-(?<month>\\d\\d)", "on 2021-05", perl = TRUE))
print(strsplit("a1b22c", "\\d+", perl = TRUE))
print(grepl("^\\p{L}+$", c("héllo", "h3"), perl = TRUE))
print(gsub("(?i)HELLO", "bye", "Hello hello", perl = TRUE))
print(gregexpr("a|b", "abcab", perl = TRUE)[[1]])
print(gsub("\\bcat\\b", "dog", "cat concat cat", perl = TRUE))
print(nchar(gsub("(*UCP)\\w", "", "héé", perl = TRUE)))
print(gsub("(?<=a)b", "X", "abab cb", perl = TRUE))
print(regexpr("a++b", "aaab", perl = TRUE))
# A pattern PCRE2 rejects warns with its message and the rest of the pattern, then errors.
tryCatch(grepl("(a", "a", perl = TRUE), warning = function(w) print(conditionMessage(w)))
tryCatch(suppressWarnings(grepl("a)", "a", perl = TRUE)), error = function(e) print(conditionMessage(e)))
