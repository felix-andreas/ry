# GNU R collates with byte order only when the collation locale is exactly "C" (POSIX reads back
# as "C"), or when the LC_ALL environment variable (LC_COLLATE when LC_ALL is empty) is exactly
# "C". C.UTF-8 and C.utf8 collate with ICU on an ICU build. The variables are read when the
# collator is set up again, which Sys.setlocale does.
words <- c("b", "B", "_x", "a", "A", "é", "f", "É", "ab", "a.b", "a_b", "Zeta", "zeta", "x10", "x2")
e <- new.env()
for (word in words) assign(word, 1, envir = e)
show <- function(label) {
  cat(label, "\n")
  print(sort(words))
  print(order(words))
  print(ls(e))
  print(sort(words, decreasing = TRUE))
}
for (locale in c("C", "POSIX", "C.UTF-8", "C.utf8")) {
  invisible(Sys.setlocale("LC_COLLATE", locale))
  show(locale)
}
Sys.setenv(LC_ALL = "C", LC_COLLATE = "C.UTF-8")
invisible(Sys.setlocale("LC_COLLATE", "C.UTF-8"))
show("LC_ALL=C over LC_COLLATE=C.UTF-8")
Sys.setenv(LC_ALL = "C.UTF-8", LC_COLLATE = "C")
invisible(Sys.setlocale("LC_COLLATE", "C.UTF-8"))
show("LC_ALL=C.UTF-8 over LC_COLLATE=C")
Sys.setenv(LC_ALL = "", LC_COLLATE = "C")
invisible(Sys.setlocale("LC_COLLATE", "C.UTF-8"))
show("empty LC_ALL, LC_COLLATE=C")
