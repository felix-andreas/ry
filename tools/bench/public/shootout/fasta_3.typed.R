alias Gene = Record{probability: F64, code: I64}

execute <- function(n: I64) -> List<Str> {
  alu <- Str.join_with([
    "GGCCGGGCGCGGTGGCTCACGCCTGTAATCCCAGCACTTTGG",
    "GAGGCCGAGGCGGGCGGATCACCTGAGGTCAGGAGTTCGAGA",
    "CCAGCCTGGCCAACATGGTGAAACCCCGTCTCTACTAAAAAT",
    "ACAAAAATTAGCCGGGCGTGGTGGCGCGCGCCTGTAATCCCA",
    "GCTACTCGGGAGGCTGAGGCAGGAGAATCGCTTGAACCCGGG",
    "AGGCGGAGGTTGCAGTGAGCCGAGATCGCGCCACTGCACTCC",
    "AGCCTGGGCGACAGAGCGAGACTCCGTCTCAAAAA"
  ], "")

  iub <- genes([
    (0.27f64, "a"), (0.12f64, "c"), (0.12f64, "g"), (0.27f64, "t"),
    (0.02f64, "B"), (0.02f64, "D"), (0.02f64, "H"), (0.02f64, "K"),
    (0.02f64, "M"), (0.02f64, "N"), (0.02f64, "R"), (0.02f64, "S"),
    (0.02f64, "V"), (0.02f64, "W"), (0.02f64, "Y")
  ])
  homosapiens <- genes([
    (0.3029549426680f64, "a"),
    (0.1979883004921f64, "c"),
    (0.1975473066391f64, "g"),
    (0.3015094502008f64, "t")
  ])

  lines <- List.append(repeat_fasta([">ONE Homo sapiens alu\n"], alu, 2 * n), ">TWO IUB ambiguity codes\n")
  two <- random_fasta(lines, iub, 3 * n, 42)
  three <- random_fasta(List.append(two$lines, ">THREE Homo sapiens frequency\n"), homosapiens, 5 * n, two$seed)
  three$lines
}

# The cumulative probabilities, each with its code as a byte.
genes <- function(table: List<Tuple{F64, Str}>) -> List<Gene> {
  total: F64 <- 0
  genes <- []
  for (entry in table) {
    total <- total + entry[[1]]
    genes <- List.append(genes, (probability = total, code = Str.to_utf8(entry[[2]])[[1]]))
  }
  genes
}

repeat_fasta <- function(lines: List<Str>, s: Str, count: I64) -> List<Str> {
  width <- 60
  chars <- Str.to_utf8(s)
  len <- List.len(chars)
  doubled <- List.concat(chars, List.take_first(chars, width))
  pos <- 1
  remaining <- count
  while (remaining > 0) {
    line <- Num.min(width, remaining)
    lines <- List.append(lines, Str.concat(Str.from_utf8_lossy(List.sublist(doubled, pos, line)), "\n"))
    pos <- pos + line
    if (pos > len) pos <- pos - len
    remaining <- remaining - line
  }
  lines
}

random_fasta <- function(lines: List<Str>, genes: List<Gene>, count: I64, seed: I64) -> Record{lines: List<Str>, seed: I64} {
  width <- 60
  remaining <- count
  while (remaining > 0) {
    line <- Num.min(width, remaining)
    rs <- List.repeat(0, line)
    for (i in List.range(1, line)) {
      seed <- (seed * 3877 + 29573) % 139968
      rs[[i]] <- Num.to_f64(seed) / 139968
    }

    # Binary search, halving every unfinished search per pass.
    n <- List.len(genes)
    lo <- List.repeat(1, line)
    hi <- List.repeat(n, line)
    mid <- List.repeat(0, line)
    inds <- List.range(1, line)
    while (!List.is_empty(inds)) {
      for (i in inds) mid[[i]] <- (lo[[i]] + hi[[i]] - 1) %/% 2
      below <- List.map2(mid, rs, \(m, r) genes[[m]]$probability < r)
      hi <- List.map3(hi, mid, below, \(h, m, b) if (b) h else m)
      lo <- List.map3(lo, mid, below, \(l, m, b) if (b) m + 1 else l)
      inds <- List.keep_if(List.range(1, line), \(i) lo[[i]] < hi[[i]])
    }
    bytes <- List.map(hi, \(index) genes[[index]]$code)
    lines <- List.append(lines, Str.concat(Str.from_utf8_lossy(bytes), "\n"))
    remaining <- remaining - line
  }
  (lines = lines, seed = seed)
}

# ---------------------------------------------------------------------------
# Driver: the same as fasta_3.R's, except that the typed program returns its lines and the
# driver prints them inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output(cat(List.to_r(execute(60000)), sep = ""))
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("fasta_3 %s %.4f\n", checksum, seconds))
})
