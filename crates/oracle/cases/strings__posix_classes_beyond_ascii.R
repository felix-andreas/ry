# The default engine's classes follow the C library's character types in a UTF-8 locale:
# another script's digit is a letter, a titlecase letter is upper and lower case, the line and
# paragraph separators are control characters, and punctuation is every graphic character that
# is not alphanumeric (marks, symbols, the no-break space, format characters).
runs <- function(codes, hit) {
    v <- codes[hit]
    if (!length(v)) return("none")
    breaks <- c(TRUE, diff(v) != 1)
    paste(sprintf("%X-%X", v[breaks], v[c(breaks[-1], TRUE)]), collapse = " ")
}
classes <- c("alpha", "digit", "alnum", "upper", "lower", "punct", "space", "blank", "cntrl",
             "graph", "print", "xdigit")

# Latin, Greek and Cyrillic, whose properties no recent Unicode release changed.
codes <- c(1:0x2FF, 0x370:0x52F)
chars <- intToUtf8(codes, multiple = TRUE)
for (class in classes) {
    pattern <- sprintf("[[:%s:]]", class)
    cat(class, ":", runs(codes, grepl(pattern, chars)), "\n")
    cat(class, "negated :", runs(codes, grepl(sprintf("[^[:%s:]]", class), chars)), "\n")
}
for (escape in c("\\w", "\\W", "\\d", "\\D", "\\s", "\\S")) {
    cat(escape, ":", runs(codes, grepl(escape, chars)), "\n")
}

# One character of each kind the classes treat differently.
samples <- c(
    "no-break space" = " ", "soft hyphen" = "­", "superscript two" = "²",
    "one half" = "½", "combining acute" = "́", "arabic-indic digit" = "١",
    "devanagari digit" = "१", "titlecase dz" = "ǅ", "greek titlecase" = "ᾈ",
    "sharp s" = "ß", "line separator" = " ", "paragraph separator" = " ",
    "en space" = " ", "narrow no-break space" = " ", "ideographic space" = "　",
    "zero width space" = "​", "zero width joiner" = "‍", "euro" = "€",
    "degree" = "°", "roman numeral" = "Ⅷ", "circled letter" = "Ⓐ",
    "han" = "日", "hiragana" = "あ", "thai vowel" = "ั", "thai mark" = "ฯ",
    "arabic number sign" = "؀", "private use" = "", "unassigned" = "͸",
    "byte order mark" = "﻿", "replacement" = "�", "emoji" = "\U0001F600",
    "mathematical bold a" = "\U0001D400", "variation selector" = "️",
    "fullwidth digit" = "１", "tag space" = "\U000E0020"
)
membership <- vapply(classes, function(class) grepl(sprintf("[[:%s:]]", class), samples), logical(length(samples)))
rownames(membership) <- names(samples)
print(ifelse(membership, "x", "."), quote = FALSE)

# The classes in use.
text <- "naïve café — €5² ١٢"
gsub("[[:punct:]]", "", text)
gsub("[[:alpha:]]+", "A", text)
gsub("[^[:alnum:][:space:]]", "", text)
regmatches(text, gregexpr("[[:upper:][:lower:]]+", text))
strsplit(text, "[[:space:]]+")
regexpr("[[:punct:]]", "a b")
grepl("^[[:graph:]]+$", c("a b", "a b", "a­b", "a b"))
toupper("ǅ ß ᾈ"); tolower("ǅ ß ᾈ")
