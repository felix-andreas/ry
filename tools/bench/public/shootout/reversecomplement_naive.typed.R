execute <- function(lines: List<Str>) -> List<Str> {
  codes <- Str.to_utf8("ACGTUMRWSYKVHDBN")
  complements <- Str.to_utf8("TGCAAKYWSRMBDHVN")
  lower_codes <- Str.to_utf8("acgtumrwsykvhdbn")
  table <- Dict.empty()
  for (i in List.range(1, List.len(codes))) {
    table <- Dict.insert(table, codes[[i]], complements[[i]])
    table <- Dict.insert(table, lower_codes[[i]], complements[[i]])
  }

  List.map(lines, \(line) {
    if (Str.starts_with(line, ">")) {
      Str.concat(line, "\n")
    } else {
      chars <- Str.to_utf8(line)
      for (j in List.range(1, List.len(chars)))
        chars[[j]] <- Result.with_default(Dict.get(table, chars[[j]]), chars[[j]])
      Str.concat(Str.from_utf8_lossy(chars), "\n")
    }
  })
}

# ---------------------------------------------------------------------------
# Driver: the same as reversecomplement_naive.R's, except that R reads the input file inside the
# clock and the typed program returns its output, which the driver prints inside the clock.
# The result is `close()`'s, as in the R program.
local({
  generator <- new.env()
  generator$.ry_bench_loading <- TRUE
  sys.source("fasta.R", envir = generator)
  scratch <- tempfile("shootout-")
  dir.create(file.path(scratch, "fasta"), recursive = TRUE)
  dir.create(file.path(scratch, "run"))
  capture.output(eval(quote(execute(100000L)), generator), file = file.path(scratch, "fasta", paste0("fasta", 100000L, ".txt")))
  home <- setwd(file.path(scratch, "run"))
  on.exit({ setwd(home); unlink(scratch, recursive = TRUE) })
  started <- proc.time()[[3]]
  output <- capture.output({
    f <- file("../fasta/fasta100000.txt", "r")
    lines <- readLines(f, warn = FALSE)
    items <- List.from_r(lines)
    cat(List.to_r(execute(items)), sep = "")
    value <- close(f)
  })
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("reversecomplement_naive %s %.4f\n", checksum, seconds))
})
