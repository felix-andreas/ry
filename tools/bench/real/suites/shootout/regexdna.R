# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=500000L) {
    pattern1 <- c(
        "agggtaaa|tttaccct",
        "[cgt]gggtaaa|tttaccc[acg]",
        "a[act]ggtaaa|tttacc[agt]t",
        "ag[act]gtaaa|tttac[agt]ct",
        "agg[act]taaa|ttta[agt]cct",
        "aggg[acg]aaa|ttt[cgt]ccct",
        "agggt[cgt]aa|tt[acg]accct",
        "agggta[cgt]a|t[acg]taccct",
        "agggtaa[cgt]|[acg]ttaccct")

    pattern2 <- matrix(c(
            c("B", "(c|g|t)"),
            c("D", "(a|g|t)"),
            c("H", "(a|c|t)"),
            c("K", "(g|t)"),
            c("M", "(a|c)"),
            c("N", "(a|c|g|t)"),
            c("R", "(a|g)"),
            c("S", "(c|g)"),
            c("V", "(a|c|g)"),
            c("W", "(a|t)"), 
            c("Y", "(c|t)")
    ), ncol=2, byrow=TRUE)

    match_count <- function(ms) {
        l <- length(ms[[1]])
        fst <- ms[[1]][[1]]
        return(if (l > 1) l else if (fst != -1L) fst else 0)
    }

    regexdna <- function(args) {
        in_filename = args[[1]]
        f <- file(in_filename, "r")
        str <- paste(c(readLines(f), ""), collapse="\n")
        close(f)

        len1 <- nchar(str)
        str <- gsub(">.*\n|\n", "", str, perl=TRUE, useBytes=TRUE)
        len2 <- nchar(str)

        for (pat in pattern1)
            cat(pat, match_count(gregexpr(pat, str, useBytes=TRUE)), "\n")

        for (i in 1:nrow(pattern2))
            str <- gsub(pattern2[[i, 1]], pattern2[[i, 2]], str, perl=TRUE, 
                        useBytes=TRUE)

        cat("", len1, len2, nchar(str), sep="\n")
    }

    file <- paste("../fasta/fasta", n, ".txt", sep="")
    regexdna(file)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
# The guard skips the driver when another program loads this file for `execute`.
if (!exists(".ry_bench_loading")) local({
  # The program reads ../fasta/fasta<n>.txt, so write that input with this
  # suite's fasta.R into a scratch directory first, outside the timed region.
  generator <- new.env()
  generator$.ry_bench_loading <- TRUE
  sys.source("fasta.R", envir = generator)
  scratch <- tempfile("shootout-")
  dir.create(file.path(scratch, "fasta"), recursive = TRUE)
  dir.create(file.path(scratch, "run"))
  capture.output(generator$execute(300000L), file = file.path(scratch, "fasta", paste0("fasta", 300000L, ".txt")))
  home <- setwd(file.path(scratch, "run"))
  on.exit({ setwd(home); unlink(scratch, recursive = TRUE) })
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(300000L))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("regexdna %s %.4f\n", checksum, seconds))
})
