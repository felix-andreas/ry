# A split sink passes output on to the sink beneath it, down to the first unsplit one.
inner <- NULL
outer <- capture.output({
  inner <- capture.output(cat("both\n"), split = TRUE)
  cat("outer only\n")
})
print(inner)
print(outer)
quiet <- capture.output({
  inner <- capture.output(cat("inner only\n"))
})
print(inner)
print(quiet)
shown <- capture.output(cat("console too\n"), split = TRUE)
print(shown)
