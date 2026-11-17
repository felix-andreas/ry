# `any` and `all` coerce every non-empty argument to logical, warning for anything but an
# integer, while `&&` and `||` take only numbers: no raw, string, NULL or factor.
probe <- function(code) {
  cat(code, "-> ")
  tryCatch(
    str(withCallingHandlers(eval(parse(text = code)), warning = function(w) {
      cat("warning:", conditionMessage(w), "| ")
      invokeRestart("muffleWarning")
    })),
    error = function(e) cat("error:", conditionMessage(e), "\n")
  )
}
r <- as.raw(1:18)
for (code in c(
  "any(r)", "all(r)", "all(as.raw(0:1))", "any(as.raw(2), 1)", "all(as.raw(2), NULL)",
  "all(\"a\")", "any(\"TRUE\")", "all(c(\"T\", \"F\"))", "any(NA_character_)", "any(NA_character_, na.rm = TRUE)",
  "any(character())", "all(list())", "any(list(TRUE))", "all(list(1, 0))", "any(list(1:2))",
  "any(\"a\", TRUE)", "all(1, \"a\")", "any(sum)",
  "r[1] && r[2]", "TRUE && r[1]", "r[1] || TRUE", "NULL && TRUE", "TRUE && NULL", "NULL || TRUE",
  "\"TRUE\" && TRUE", "\"a\" || TRUE", "factor(\"x\") && TRUE", "FALSE || factor(\"x\")",
  "logical() && TRUE", "1i && TRUE", "c(TRUE, FALSE) && TRUE", "list(TRUE) && TRUE"
)) probe(code)
f <- function(x, y) x && y
for (i in 1:3) probe("f(r[1], r[2])")
for (i in 1:3) probe("f(TRUE, NULL)")
g <- function(x) any(x)
for (i in 1:3) probe("g(r)")
