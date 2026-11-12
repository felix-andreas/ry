execute <- function(lines: List<Str>) -> List<Str> {
  count <- List.len(lines)
  i <- 1
  while (i <= count && !Str.starts_with(lines[[i]], ">TH")) i <- i + 1
  i <- i + 1
  parts <- []
  while (i <= count && !Str.starts_with(lines[[i]], ">") && !Str.starts_with(lines[[i]], ";")) {
    parts <- List.append(parts, lines[[i]])
    i <- i + 1
  }
  sequence <- Str.to_utf8(Str.join_with(parts, ""))

  output <- [sorted_frequencies(sequence, 1), sorted_frequencies(sequence, 2)]
  for (s in ["GGT", "GGTA", "GGTATT", "GGTATTTTAATT", "GGTATTTTAATTTATAGT"]) {
    lower <- Str.with_ascii_lowercased(s)
    found <- Result.with_default(Dict.get(frequencies(sequence, List.len(Str.to_utf8(lower))), lower), 0i64)
    output <- List.append(output, f"{found}\t{s}\t\n")
  }
  output
}

frequencies <- function(sequence: List<I64>, frame: I64) -> Dict<Str, I64> {
  counts <- Dict.empty()
  for (i in List.range(1, List.len(sequence) - frame + 1)) {
    key <- Str.from_utf8_lossy(List.sublist(sequence, i, frame))
    counts <- Dict.update(counts, key, function(found) :Ok(Result.with_default(found, 0i64) + 1i64))
  }
  counts
}

sorted_frequencies <- function(sequence: List<I64>, frame: I64) -> Str {
  entries <- Dict.to_list(frequencies(sequence, frame))
  total <- Num.to_f64(List.sum(List.map(entries, \(entry) entry[[2]])))
  sorted <- List.sort_with(entries, \(a, b) {
    match (Num.compare(b[[2]], a[[2]])) {
      :EQ => if (a[[1]] < b[[1]]) :LT else :GT
      order => order
    }
  })
  lines <- List.map(sorted, \(entry) f"{entry[[1]]} {100 * Num.to_f64(entry[[2]]) / total}")
  Str.concat(Str.join_with(lines, "\n"), " \n")
}

# ---------------------------------------------------------------------------
# Driver: the same as knucleotide.R's, except that R reads the input file inside the clock
# and the typed program returns its output, which the driver prints inside the clock.
local({
  generator <- new.env()
  generator$.ry_bench_loading <- TRUE
  sys.source("fasta.R", envir = generator)
  scratch <- tempfile("shootout-")
  dir.create(file.path(scratch, "fasta"), recursive = TRUE)
  dir.create(file.path(scratch, "run"))
  capture.output(eval(quote(execute(4000L)), generator), file = file.path(scratch, "fasta", paste0("fasta", 4000L, ".txt")))
  home <- setwd(file.path(scratch, "run"))
  on.exit({ setwd(home); unlink(scratch, recursive = TRUE) })
  started <- proc.time()[[3]]
  output <- capture.output({
    f <- file("../fasta/fasta4000.txt", "r")
    print(f)
    lines <- readLines(f, warn = FALSE)
    close(f)
    items <- List.from_r(lines)
    cat(List.to_r(execute(items)), sep = "")
  })
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("knucleotide %s %.4f\n", checksum, seconds))
})
