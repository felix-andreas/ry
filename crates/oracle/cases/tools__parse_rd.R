# tools::parse_Rd builds a tagged list tree from Rd markup. This prints every leaf with its tag,
# srcref and extra attributes, then round-trips trees through as.character() and renders a
# small document as text and HTML.

show_rd <- function(x, indent = "") {
  a <- attributes(x)
  tag <- if (is.null(a$Rd_tag)) "<none>" else a$Rd_tag
  where <- if (is.null(a$srcref)) "" else paste0(" @", paste(as.integer(a$srcref), collapse = ","))
  extra <- setdiff(names(a), c("Rd_tag", "srcref", "names", "macros", "class", "Rd_option"))
  extras <- vapply(extra, function(n) paste0(n, "=", paste(format(a[[n]]), collapse = ",")), "")
  if (is.list(x)) {
    cat(indent, "LIST ", tag, where, if (length(extras)) paste0(" [", paste(extras, collapse = " "), "]"), "\n", sep = "")
    if (!is.null(a$Rd_option)) show_rd(a$Rd_option, paste0(indent, "  option: "))
    for (e in x) show_rd(e, paste0(indent, "  "))
  } else {
    cat(indent, tag, " ", deparse(as.vector(unclass(x))), where,
        if (length(extras)) paste0(" [", paste(extras, collapse = " "), "]"), "\n", sep = "")
  }
}

rd_text <- function(text, fragment = TRUE) {
  cat("==== ", deparse(text), "\n", sep = "")
  rd <- withCallingHandlers(
    tools::parse_Rd(textConnection(text), fragment = fragment),
    warning = function(w) {
      cat("warning:", conditionMessage(w), "\n")
      invokeRestart("muffleWarning")
    }
  )
  show_rd(rd)
  cat("macros:", sort(ls(attr(rd, "macros"))), "\n")
  invisible(rd)
}

rd_text("plain {group} text\nsecond line \\% \\\\ \\{ done")
rd_text("\\emph{a} \\bold{b}{extra} \\link[pkg:topic]{t} \\sQuote{\\R}\\dots{}x")
rd_text("\\code{x <- \"a}b\"; f('{') # c \\% d % comment\ny \\link{z}}")
rd_text("\\verb{a \\{ \\} \\\\ \\% \\q {b} % c\n} \\samp{a{b}c} \\url{http://x.y/}")
rd_text("\\eqn{a % b \\{}{a} \\deqn{x} \\figure{f.png}{width=1}")
rd_text("\\itemize{\\item one\n\\item two} \\describe{\\item{a}{b}}")
rd_text("\\tabular{ll}{a \\tab b\\cr c \\tab d}")
rd_text("\\if{html}{\\out{<hr>}}\\ifelse{a}{b}{c}\\href{https://r-project.org}{site}")
rd_text("\\method{print}{foo}(x) \\S4method{show}{bar}(object) \\enc{Jöreskog}{Joreskog}")
rd_text("\\Sexpr[results=rd]{x <- \"}\"; 1} \\donttest{a\nb} \\dontrun{c \\% {d}}")
rd_text("x\n#ifdef unix\n\\code{a}\n#endif\n#ifndef windows\nb\n#endif\ny")
rd_text("\\newcommand{\\pair}{[#1|#2]}\\pair{a{b}c}{d % e\n} \\pair{}{z}")
rd_text("\\newcommand{\\one}{X}\\renewcommand{\\one}{Y}\\newcommand{\\two}{\\one\\one}\\two \\code{\\two}")
rd_text("\\CRANpkg{foo} and \\doi{10.1/x}")
rd_text("% a comment\ntext % another\n")
rd_text("\\unknownthing{x} and \\title{t}")

doc <- rd_text("\\name{fn}\n\\alias{fn}\n\\title{The \\emph{fn} function}\n\\description{\nDoes things.\n\nTwo paragraphs.\n}\n\\usage{\nfn(fmt = \"\\%s\")\n\\method{print}{fn}(x, \\dots)\n}\n\\arguments{\n  \\item{fmt}{a format, see \\code{\\link{sprintf}}}\n}\n\\value{A list \\item{a}{first}}\n\\section{Note}{See \\url{https://example.org}.}\n\\examples{\nfn(1) # hi\n\\dontrun{\nfn(2)}\n}\n\\keyword{misc}\n", fragment = FALSE)

cat(as.character(doc), sep = "")
cat(as.character(doc, deparse = TRUE), sep = "")
for (text in c("\\link[=Paren]{\\{}", "a {b} c", "\\code{\"q\\\\\" {x}}", "100\\% \\\\ back")) {
  rd <- tools::parse_Rd(textConnection(text), fragment = TRUE)
  print(paste(as.character(rd, deparse = TRUE), collapse = ""))
}

for (state in list(c(0L, 0L, 1L, 0L, 0L), c(0L, 0L, 2L, 0L, 0L), c(0L, 0L, 3L, 1L, 1L), c(1L, 0L, 1L, 0L, 0L))) {
  for (text in c("a{b}c", "}", "x %\\ y", "'s{' \"t\\\"}\" `u{`")) {
    out <- tools:::deparseRdElement(text, state)
    cat(sprintf("%-22s %-10s -> %-28s %s\n", deparse(text), paste(state, collapse = ""), deparse(out[[1]]), paste(out[[2]], collapse = ",")))
  }
}

tools::Rd2txt(doc, out <- textConnection(NULL, "w"))
writeLines(textConnectionValue(out)); close(out)
