# `toupper` and `tolower` map one character to one character. A character whose full case
# mapping is several characters changes only where Unicode gives it a simple mapping.
codes <- function(x) vapply(x, function(s) paste(sprintf("%04X", utf8ToInt(s)), collapse = " "), "",
                            USE.NAMES = FALSE)
dotted <- "İ"
codes(tolower(dotted))
codes(casefold(c("ıİ ǅ", "Iİiı")))
codes(toupper(c("ı", "i", "ǅ", "ǆ")))

# Greek letters with an iota subscript have a titlecase form as their upper case.
iota <- intToUtf8(c(0x1f80:0x1f87, 0x1f90:0x1f97, 0x1fa0:0x1fa7, 0x1fb3, 0x1fc3, 0x1ff3), multiple = TRUE)
codes(toupper(iota))
identical(tolower(toupper(iota)), iota)
codes(toupper(paste(iota[c(1, 25)], collapse = "")))

# No simple mapping: the character is left alone.
alone <- c("ß", "ŉ", "ǰ", "ΐ", "ΰ", "և", "ẖ", "ὐ", "ﬀ", "ﬆ")
identical(toupper(alone), alone)
codes(toupper("straße ﬁn"))

# A latin1 string maps the same way.
x <- iconv("café ßÿ", "UTF-8", "latin1")
codes(toupper(x)); Encoding(toupper(x))
