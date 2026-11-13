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
  doubled <- List.repeat(0, len + width)
  for (i in List.range(1, len)) doubled[[i]] <- chars[[i]]
  for (i in List.range(1, width)) doubled[[len + i]] <- chars[[i]]
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
  lookup_size <- 4096
  lookup_scale <- Num.to_f64(lookup_size - 1)
  n <- List.len(genes)
  cprob_lookup <- List.map(genes, \(gene) gene$probability * lookup_scale)
  cprob_lookup[[n]] <- Num.to_f64(lookup_size - 1)

  lookup <- List.repeat(0, lookup_size)
  j <- 1
  for (i in List.range(1, lookup_size)) {
    while (cprob_lookup[[j]] + 1 < Num.to_f64(i)) j <- j + 1
    lookup[[i]] <- j
  }

  remaining <- count
  while (remaining > 0) {
    line <- Num.min(width, remaining)
    bytes <- List.repeat(0, line)
    for (i in List.range(1, line)) {
      seed <- (seed * 3877 + 29573) %% 139968
      r <- Num.to_f64(seed) * (lookup_scale / 139968)
      ind <- lookup[[Num.truncate(r) + 1]]
      while (cprob_lookup[[ind]] < r) ind <- ind + 1
      bytes[[i]] <- genes[[ind]]$code
    }
    lines <- List.append(lines, Str.concat(Str.from_utf8_lossy(bytes), "\n"))
    remaining <- remaining - line
  }
  (lines = lines, seed = seed)
}

# ---------------------------------------------------------------------------
# Driver: the same as fastaredux_naive.R's, except that the typed program returns its lines and the
# driver prints them inside the clock.
local({
  started <- proc.time()[[3]]
  output <- capture.output(cat(List.to_r(execute(80000)), sep = ""))
  seconds <- proc.time()[[3]] - started
  value <- NULL
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("fastaredux_naive %s %.4f\n", checksum, seconds))
})
