# strwidth() and strheight() in figure units or inches need no plot.new(); user units do.
pdf(NULL)
print(tryCatch(strwidth("abc", "figure"), error = conditionMessage))
print(tryCatch(strheight("abc", "figure"), error = conditionMessage))
print(tryCatch(strwidth("abc"), error = conditionMessage))
print(tryCatch(strwidth(c("a", "abc"), "inches", cex = 2), error = conditionMessage))
print(tryCatch(strheight(expression(x^2), "inches"), error = conditionMessage))
invisible(dev.off())
