# typed ry has no regular expressions, so the port matches the program's patterns by hand.
# Each pattern is a choice of fixed-length alternatives whose positions are a literal letter or
# a class such as [cgt], and the port parses them into that shape: an alternative is a list of
# positions, and a position is the list of bytes it accepts. The other two regular expressions
# are plain substitutions: ">.*\n|\n" drops header lines and line breaks, and each IUB code is
# replaced by its alternation.
alias Alternative = List<List<I64>>

execute <- function(lines: List<Str>) -> List<Str> {
  pattern1 <- [
    "agggtaaa|tttaccct",
    "[cgt]gggtaaa|tttaccc[acg]",
    "a[act]ggtaaa|tttacc[agt]t",
    "ag[act]gtaaa|tttac[agt]ct",
    "agg[act]taaa|ttta[agt]cct",
    "aggg[acg]aaa|ttt[cgt]ccct",
    "agggt[cgt]aa|tt[acg]accct",
    "agggta[cgt]a|t[acg]taccct",
    "agggtaa[cgt]|[acg]ttaccct"
  ]
  pattern2 <- [
    ("B", "(c|g|t)"), ("D", "(a|g|t)"), ("H", "(a|c|t)"), ("K", "(g|t)"),
    ("M", "(a|c)"), ("N", "(a|c|g|t)"), ("R", "(a|g)"), ("S", "(c|g)"),
    ("V", "(a|c|g)"), ("W", "(a|t)"), ("Y", "(c|t)")
  ]

  str <- Str.join_with(List.append(lines, ""), "\n")
  len1 <- Str.count_utf8_bytes(str)
  str <- drop_headers(str)
  len2 <- Str.count_utf8_bytes(str)

  output <- []
  bytes <- Str.to_utf8(str)
  for (pat in pattern1) {
    output <- List.append(output, f"{pat} {match_count(bytes, parse_pattern(pat))} \n")
  }

  for (substitution in pattern2)
    str <- Str.replace_each(str, substitution[[1]], substitution[[2]])

  List.append(output, f"\n{len1}\n{len2}\n{Str.count_utf8_bytes(str)}")
}

# gsub(">.*\n|\n", "", str)
drop_headers <- function(str: Str) -> Str {
  bytes <- Str.to_utf8(str)
  newline <- 10i64
  kept <- List.with_capacity(List.len(bytes))
  i <- 1i64
  while (i <= List.len(bytes)) {
    if (bytes[[i]] == 62) {
      while (i <= List.len(bytes) && bytes[[i]] != newline) i <- i + 1
    } else if (bytes[[i]] != newline) {
      kept <- List.append(kept, bytes[[i]])
    }
    i <- i + 1
  }
  Str.from_utf8_lossy(kept)
}

parse_pattern <- function(pattern: Str) -> List<Alternative> {
  List.map(Str.split_on(pattern, "|"), \(alternative) {
    bytes <- Str.to_utf8(alternative)
    positions <- []
    i <- 1i64
    while (i <= List.len(bytes)) {
      if (bytes[[i]] == 91) {
        class <- []
        i <- i + 1
        while (bytes[[i]] != 93) {
          class <- List.append(class, bytes[[i]])
          i <- i + 1
        }
        positions <- List.append(positions, class)
      } else {
        positions <- List.append(positions, [bytes[[i]]])
      }
      i <- i + 1
    }
    positions
  })
}

# The R program's match_count(gregexpr(pat, str)): the number of non-overlapping matches, except
# that a single match counts as its position, as written upstream.
match_count <- function(bytes: List<I64>, alternatives: List<Alternative>) -> I64 {
  count <- 0i64
  first <- 0i64
  i <- 1i64
  while (i <= List.len(bytes)) {
    found <- List.find_first(alternatives, \(alternative) matches_at(bytes, i, alternative))
    match (found) {
      :Ok(alternative) => {
        count <- count + 1
        if (first == 0) first <- i
        i <- i + List.len(alternative)
      }
      :Err(_) => {
        i <- i + 1
      }
    }
  }
  if (count > 1) count else first
}

matches_at <- function(bytes: List<I64>, start: I64, alternative: Alternative) -> Bool {
  if (start + List.len(alternative) - 1 > List.len(bytes)) return(Bool.false)
  k <- 1i64
  while (k <= List.len(alternative)) {
    if (!List.contains(alternative[[k]], bytes[[start + k - 1]])) return(Bool.false)
    k <- k + 1
  }
  Bool.true
}

# ---------------------------------------------------------------------------
# Driver: the same as regexdna.R's, except that R reads the input file inside the clock and
# the typed program returns its output, which the driver prints inside the clock.
local({
  generator <- new.env()
  generator$.ry_bench_loading <- TRUE
  sys.source("fasta.R", envir = generator)
  scratch <- tempfile("shootout-")
  dir.create(file.path(scratch, "fasta"), recursive = TRUE)
  dir.create(file.path(scratch, "run"))
  capture.output(eval(quote(execute(300000L)), generator), file = file.path(scratch, "fasta", paste0("fasta", 300000L, ".txt")))
  home <- setwd(file.path(scratch, "run"))
  on.exit({ setwd(home); unlink(scratch, recursive = TRUE) })
  started <- proc.time()[[3]]
  output <- capture.output({
    f <- file("../fasta/fasta300000.txt", "r")
    lines <- readLines(f)
    close(f)
    items <- List.from_r(lines)
    value <- cat(List.to_r(execute(items)), sep = "")
  })
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("regexdna %s %.4f\n", checksum, seconds))
})
