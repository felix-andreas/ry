# Corners of GNU R's grammar that the parser kind of the differential fuzzer found: where `?`
# ends a body, hexadecimal fractions, the mark of a string literal with byte escapes, where a
# lambda's `\` reads across a line break, and what a syntax error calls `\` and `**`.
show <- function(s) {
  warnings <- character()
  result <- withCallingHandlers(
    tryCatch({
      expressions <- parse(text = s, keep.source = FALSE)
      paste(vapply(expressions, function(e) paste(deparse(e), collapse = " "), ""), collapse = " ; ")
    }, error = function(e) paste("error:", sub("\n.*", "", conditionMessage(e)))),
    warning = function(w) { warnings <<- c(warnings, conditionMessage(w)); invokeRestart("muffleWarning") })
  cat(encodeString(s), "->", result, "\n")
  for (w in warnings) cat("  warning:", w, "\n")
}

# `?` binds looser than a function, a loop and a branch: their bodies end before it.
for (s in c("function(x) x ? f", "\\(x) x ? f", "if (a) b ? c", "if (a) b else c ? d", "while (a) b ? c",
            "for (i in a) b ? c", "repeat b ? c", "while (a) b = c ? d", "? a = b", "a ? b ? c", "f(a ? b)",
            "if (a ? b) c", "x <- function(x) x ? f", "-a ? b", "a = b ? c = d")) {
  e <- parse(text = s, keep.source = FALSE)[[1]]
  cat(encodeString(s), "->", as.character(e[[1]]), length(e), "\n")
}

# A hexadecimal literal takes a fraction with or without a `p` exponent.
for (s in c("0x1.8", "0x.8", "0x1.", "0x.", "0x.p1", "0x1.8p1", "0xA.8P-1", "0x1.1L", "0x1.0L", "0x1p1L",
            "0x1.8p1L", "0x1.8i")) {
  show(s)
  value <- tryCatch(suppressWarnings(eval(parse(text = s))), error = function(e) NULL)
  if (!is.null(value)) cat("  ", typeof(value), format(value, digits = 17), "\n")
}

# An octal or hexadecimal escape leaves the literal unmarked, a `\u` escape marks it UTF-8, and
# a literal with both is an error at its closing quote.
for (s in c("'\\x41'", "'\\101'", "'ä'", "'ä\\x41'", "'\\u00e4'", "'\\u0041'", "'\\xe4'", "'\\303\\244'",
            "'\\u00e4\\x41'", "'\\101\\U0001F600'", "'\\u0041\\101'", "\"\\u00e4\\x41", "'\\x41\\u00e4")) {
  show(s)
  value <- tryCatch(eval(parse(text = s)), error = function(e) NULL)
  if (!is.null(value)) cat("  ", Encoding(value), paste(charToRaw(value), collapse = " "), "\n")
}

# A lambda's `\` reads across a line break wherever the lexer skips one before it: after an
# operator, a keyword, a head and `{`, and never at the start of a later statement.
for (s in c("\\\n(x) x", "x\n\\\n(x) x", "x; \\\n(x) x", "{\\\n(x) x}", "{\n\\\n(x) x}", "{;\\\n(x) x}", "{x\n\\\n(x) x}",
            "{}\n\\\n(x) x", "x <- \\\n(x) x", "x = \\\n(x) x", "-\n\\\n\n(x) x", "repeat \\\n(x) x", "if (a) \\\n(x) x",
            "if (a) b else \\\n(x) x", "function(y)\n\\\n(x) x", "\\(y) \\\n(x) x", "f(\\\n(x) x)", "(\\\n(x) x)",
            "function\n(x) x")) show(s)

# `::` and `:::` end the statement at a line break; `$` and `@` do not.
for (s in c("a::\nb", "a:::\nb", "a:: # c\nb", "(a::\nb)", "{a::\nb}", "a$\nb", "a@\nb", "a :: b")) show(s)

# A number the lexer refuses and a character that starts no token are "unexpected input" where
# they start, and a control character is an "invalid token".
for (s in c("0x", "0X", "0xg", "0xp1", "0x1..2", "0x1.2.3", "0x1.8p", "0x1p+", "0x1pL", "x <- 0x", "1 + 0x + 2",
            "1e", "1e+", "1ex", "x <- 3e", "f(1e-)", "c(1E, 2)", "1.e", ".5e", "if (TRUE) 1else 2", "1e5else",
            "1e5.5", "1.2.3", "1..2", "1 %foo", "a \u00a7 b", "1 + \U0001F600", "x <- 1 \001 2", "x \033")) {
  show(s)
}

# `...` and `..1` are loop variables.
for (s in c("for (... in x) a", "for (..1 in x) a", "for (`a b` in x) a", "for ('a' in x) a")) show(s)
f <- function(...) { for (..1 in 1:2) cat(get("..1"), ""); ..1 }
print(f(9))

# A syntax error names `\` and `**` by their tokens.
for (s in c("x \\", "1 \\(x) x", "a ** ** b", "x **")) show(s)
