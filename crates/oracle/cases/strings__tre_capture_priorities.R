# POSIX capture groups follow TRE's tag priorities: a later repeated or optional group can take
# the text an earlier one could have, and the first alternative that fits wins over a longer one.
groups <- function(pattern, text) print(regmatches(text, regexec(pattern, text))[[1]])
groups("(a)?(ab)?b*", "abb")
groups("(a*)(ab)?b*", "abb")
groups("(a|ab)(bc|c)?", "abc")
groups("(a|ab)(c|bcd)(d*)", "abcd")
groups("(a?)((ab)?)(b?)", "ab")
groups("(aa)?(aab)?b*", "aabb")
groups("([ab])|b|(c)", "ba")
groups("(a|b)*c", "abac")
groups("x([0-9]+)-([0-9]*)?", "x12-")
# A minimal repetition inside a group decides where the whole match ends.
groups("(b?a(c*?b)*)*", "acaa")
groups("a(.*?)b.*", "aXbXb")
print(regmatches("xabcd", regexpr("xabcd|b*?c", "xabcd")))
# A group repeated zero times still counts, as the empty string at the start of the match.
groups("(a){0}c", "bcac")
# Back-references in a replacement use the same groups.
print(sub("(a)?(ab)?b*", "<\\1|\\2>", "abb"))
print(gsub("(a|ab)(c|bcd)", "[\\1,\\2]", "abcd abc"))
