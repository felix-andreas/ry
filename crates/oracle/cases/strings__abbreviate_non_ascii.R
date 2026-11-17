# abbreviate() warns about non-ASCII text only when it shortens such a string.
show <- function(expr) withCallingHandlers(
    dput(expr),
    warning = function(w) {
        cat("warning:", conditionMessage(w), "\n")
        invokeRestart("muffleWarning")
    }
)
show(abbreviate("ü", 4))
show(abbreviate("abcdefghü", 12))
show(abbreviate(c("ab", "ü"), 1))
show(abbreviate("überlang und mehr", 4))
show(abbreviate(c("überlang", "international", "été"), 5))
show(abbreviate("abcdefgh üijkl", 5, named = FALSE))
show(abbreviate("üäö", 3))
show(abbreviate("üäö", 2))
show(abbreviate("ü", 0))
show(abbreviate(c("über alles", "über allem"), 4, method = "both.sides"))
show(abbreviate(c("über alles", "über allem"), 4, strict = TRUE))
show(abbreviate("Überlänge Straße", 6, use.classes = FALSE))
show(abbreviate("Überlänge Straße", 6, dot = TRUE))
latin <- "\xfcberlang"
Encoding(latin) <- "latin1"
show(abbreviate(latin, 4))
show(abbreviate(latin, 20))
show(.Internal(abbreviate(" ü ", 1L, TRUE)))
show(.Internal(abbreviate(c(NA, "ü"), 1L, TRUE)))

# The passes: vowels at a word's end, vowels, lower-case letters at a word's end, lower-case
# letters, then any character after a word's first.
words <- c("international business machines", "a e i o u", "queue", "aaaa bbbb", "ABCDEFGH",
           "Mississippi River", "x", "to be or not to be", "tab\tseparated words", "end.of-line",
           "éléphant rose", "řešení úlohy", "naïve café",
           "日本語 文字列", "no break space")
for (minlength in c(1, 2, 3, 5, 8, 12)) {
    show(abbreviate(words, minlength, named = FALSE))
    show(abbreviate(words, minlength, use.classes = FALSE, named = FALSE))
    show(abbreviate(words, minlength, strict = TRUE, named = FALSE))
}
show(abbreviate(words, 4, method = "both.sides", named = FALSE))
show(abbreviate(c("  padded  ", "dup", "dup", NA), 2))

# The internal function keeps attributes, and treats a negative minimum as an unsigned number.
show(.Internal(abbreviate(structure(c(a = "alpha beta", b = "über uns"), class = "tag"), 3L, TRUE)))
show(.Internal(abbreviate(matrix(c("alpha", "beta", "gamma", "delta"), 2), 2L, FALSE)))
show(.Internal(abbreviate(c("one two", "ün deux", " lead ", " ü x "), -1L, TRUE)))
show(.Internal(abbreviate(c("one two", "ün deux"), 0L, TRUE)))
show(tryCatch(.Internal(abbreviate("a", 1L, NA)), error = conditionMessage))
show(tryCatch(.Internal(abbreviate("a", NA, TRUE)), error = conditionMessage))
show(Encoding(abbreviate(c(latin, "überlang", "plain text"), 4)))
show(Encoding(abbreviate(c(latin, "überlang", "plain text"), 40)))
