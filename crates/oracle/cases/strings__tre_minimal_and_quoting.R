# TRE's minimal repetition: the shortest whole match unless a capture group holds or follows it.
m <- function(p, x) regmatches(x, regexpr(p, x))
m("a.*?b.*", "aXbXb")
m("a.*?b+", "aXbbb")
m("(a).*?b.*", "aXbXb")
m("a(.*?)b.*", "aXbXb")
m("a.*?(b).*", "aXbXb")
m("a(.+?)b+", "aXbbb")
m("x(.+?)(a+)", "xaaab")
pem <- "-----BEGIN C-----\nAAA\n-----END C-----\n-----BEGIN C-----\nB\n-----END C-----\n"
regmatches(pem, gregexpr("(-+BEGIN)(.+?)(-+END)(.+?)(-+)", pem))[[1]]

# \Q...\E quotes, a lone \E is a literal E, and a ) that closes no group is a literal.
grepl("a\\Q.*\\Eb", c("a.*b", "axxb"))
grepl("a\\Eb", c("ab", "aEb"))
grepl("\\breactlog\\s+\\Q(>= 1.0.0)\\E", "ggplot2, reactlog\n (>= 1.0.0)")
gsub("\\([0-9a-f]{1,6})(?:\r\n|[ \n\r\t\f])?", "<\\1>", c("(41) b", "(41 b", "x41)"), ignore.case = TRUE)
gsub("a)b", "X", c("a)b", "ab"))

# grepRaw compiles in byte mode, where \s and [[:space:]] are ASCII.
grepRaw("^(ssh|ecdsa)-[a-z0-9-]+\\s+", charToRaw("ssh-rsa AAAA"))
grepRaw("[[:space:]]+", charToRaw("ab  c"), value = TRUE)
grepRaw("^(ssh|ecdsa)-[a-z0-9-]+\\s+", charToRaw("SSH-rsa AAAA"), ignore.case = TRUE)
