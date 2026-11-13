# The R program counts each k-mer in an attribute of an integer vector, and attr() searches
# the attributes in order, so the port keeps the counts in an association list it searches the
# same way.
execute <- function(lines: List<Str>) -> List<Str> {
  count <- List.len(lines)
  i <- 1i64
  while (i <= count && !Str.starts_with(lines[[i]], ">TH")) i <- i + 1
  i <- i + 1
  parts <- []
  while (i <= count && !Str.starts_with(lines[[i]], ">") && !Str.starts_with(lines[[i]], ";")) {
    parts <- List.append(parts, lines[[i]])
    i <- i + 1
  }
  sequence <- Str.to_utf8(Str.join_with(parts, ""))

  output <- [sort_seq(sequence, 1), sort_seq(sequence, 2)]
  for (s in ["GGT", "GGTA", "GGTATT", "GGTATTTTAATT", "GGTATTTTAATTTATAGT"]) {
    output <- List.append(output, f"{find_seq(sequence, Str.with_ascii_lowercased(s))}\t{s}\t\n")
  }
  output
}

sort_seq <- function(sequence: List<I64>, frame: I64) -> Str {
  entries <- gen_freq(sequence, frame)
  total <- Num.to_f64(List.sum(List.map(entries, \(entry) entry[[2]])))
  upper <- List.map(entries, \(entry) (Str.with_ascii_uppercased(entry[[1]]), entry[[2]]))
  sorted <- List.sort_with(upper, \(a, b) {
    match (Num.compare(b[[2]], a[[2]])) {
      :EQ => if (a[[1]] < b[[1]]) :LT else :GT
      order => order
    }
  })
  lines <- List.map(sorted, \(entry) f"{entry[[1]]} {100 * Num.to_f64(entry[[2]]) / total}")
  Str.concat(Str.join_with(lines, "\n"), " \n")
}

find_seq <- function(sequence: List<I64>, s: Str) -> I64 {
  freqs <- gen_freq(sequence, List.len(Str.to_utf8(s)))
  match (List.find_first_index(freqs, \(entry) entry[[1]] == s)) {
    :Ok(index) => freqs[[index]][[2]]
    :Err(_) => 0i64
  }
}

gen_freq <- function(sequence: List<I64>, frame: I64) -> List<Tuple{Str, I64}> {
  freqs <- []
  for (i in List.range(1, List.len(sequence) - frame + 1)) {
    subseq <- Str.from_utf8_lossy(List.sublist(sequence, i, frame))
    match (List.find_first_index(freqs, \(entry) entry[[1]] == subseq)) {
      :Ok(index) => {
        freqs[[index]] <- (subseq, freqs[[index]][[2]] + 1)
      }
      :Err(_) => {
        freqs <- List.append(freqs, (subseq, 1i64))
      }
    }
  }
  freqs
}

# ---------------------------------------------------------------------------
# Driver: the same as knucleotide_brute.R's, except that R reads the input file inside the clock
# and the typed program returns its output, which the driver prints inside the clock.
local({
  generator <- new.env()
  generator$.ry_bench_loading <- TRUE
  sys.source("fasta.R", envir = generator)
  scratch <- tempfile("shootout-")
  dir.create(file.path(scratch, "fasta"), recursive = TRUE)
  dir.create(file.path(scratch, "run"))
  capture.output(eval(quote(execute(1000L)), generator), file = file.path(scratch, "fasta", paste0("fasta", 1000L, ".txt")))
  home <- setwd(file.path(scratch, "run"))
  on.exit({ setwd(home); unlink(scratch, recursive = TRUE) })
  started <- proc.time()[[3]]
  output <- capture.output({
    f <- file("../fasta/fasta1000.txt", "r")
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
  cat(sprintf("knucleotide_brute %s %.4f\n", checksum, seconds))
})
