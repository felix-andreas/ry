# Help pages render to text, HTML and LaTeX, and print through file.show() as GNU R's pager does.
rd <- utils:::.getHelpFile(help("mean", package = "base"))
tools::Rd2txt(rd, options = list(underline_titles = FALSE))
html <- capture.output(tools::Rd2HTML(rd, out = ""))
length(html)
html[1:4]
head(capture.output(tools::Rd2latex(rd, out = stdout())), 6)
tools::Rd2ex(rd)
tools::checkRd(rd)

shown <- tempfile()
writeLines(c("first", "second"), shown)
file.show(shown, header = "A header", title = "ignored", delete.file = TRUE)
file.exists(shown)
file.show("/nonexistent/help-page")

print(help("sum", package = "base", help_type = "text"))
help(package = "tools")$info[[1]][1:3]
