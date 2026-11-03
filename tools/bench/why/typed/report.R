# String building: format one report line per account, then join and measure the report.
report <- function(n: integer) -> integer {
  lines <- character(n)
  for (i in seq_len(n)) {
    name <- paste0("ACC", formatC(i, width = 6L, flag = "0"))
    side <- if (i %% 2L == 0L) "long" else "short"
    lines[[i]] <- paste(name, sprintf("%.2f", i * 1.25), side, sep = ";")
  }
  text <- paste(lines, collapse = "\n")
  nchar(text) + sum(nchar(toupper(lines)) > 25L) + sum(startsWith(lines, "ACC00"))
}
measure("report", function() report(20000L))
