# The R program counts each k-mer in a named integer vector, which `[` searches by name in
# order, so the port keeps the names and the counts in two lists it searches the same way. The
# vector starts with 16 unnamed zeros, and each new k-mer also writes a zero at the vector's
# doubling capacity, growing it when needed; that write can land on a counted k-mer, as in R.
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
  freqs <- gen_freq(sequence, frame)
  entries <- List.keep_if(List.map2(freqs$names, freqs$counts, \(name, count) (name, count)), \(entry) entry[[2]] > 0)
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
  match (List.find_first_index(freqs$names, \(name) name == s)) {
    :Ok(index) => freqs$counts[[index]]
    :Err(_) => 0i64
  }
}

gen_freq <- function(sequence: List<I64>, frame: I64) -> Record{names: List<Str>, counts: List<I64>} {
  n <- 0i64
  cap <- 16i64
  names <- List.repeat("", cap)
  counts <- List.repeat(0i64, cap)
  for (i in List.range(1, List.len(sequence) - frame + 1)) {
    subseq <- Str.from_utf8_lossy(List.sublist(sequence, i, frame))
    index <- Result.with_default(List.find_first_index(names, \(name) name == subseq), 0i64)
    cnt <- if (index == 0i64) 0i64 else counts[[index]]
    if (index == 0i64) {
      n <- n + 1
      if (cap < n) cap <- 2 * cap
      if (cap > List.len(counts)) {
        names <- List.concat(names, List.repeat("", cap - List.len(names)))
        counts <- List.concat(counts, List.repeat(0i64, cap - List.len(counts)))
      }
      counts[[cap]] <- 0i64
      names <- List.append(names, subseq)
      counts <- List.append(counts, cnt + 1)
    } else {
      counts[[index]] <- cnt + 1
    }
  }
  (names = names, counts = counts)
}

# ---------------------------------------------------------------------------
# Driver: the same as knucleotide_brute_3.R's, except that R reads the input file inside the clock
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
  cat(sprintf("knucleotide_brute_3 %s %.4f\n", checksum, seconds))
})
