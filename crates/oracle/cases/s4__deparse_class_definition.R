# deparse() of an S4 object names the slots of its own class, so a class definition
# deparses without recursing into the class it describes, with GNU R's line breaks.
ext <- methods::getClass("MethodDefinition")@contains[[1]]
writeLines(deparse(ext))
setClass("Point", representation(x = "numeric", y = "numeric"))
p <- new("Point", x = c(1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5), y = c(10, 20, 30, 40, 50, 60, 70))
writeLines(deparse(p))
writeLines(deparse(list(p, list(a = "a long string to push the line past the cutoff", b = p))))
d <- deparse(methods::getClass("MethodDefinition"), nlines = 3L)
print(length(d))
