# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=2000) {
    gen_freq <- function(seq, frame) {
        frame <- frame - 1L
        ns <- length(seq) - frame
        h <- new.env(emptyenv(), hash=TRUE)
        for (i in 1:ns) {
            subseq_str = paste(seq[i:(i + frame)], collapse="", sep="")
      if (exists(subseq_str, h, inherits=FALSE))
          cnt <- get(subseq_str, h, inherits=FALSE)
      else
          cnt <- 0L
      assign(subseq_str, cnt + 1L, h)
        }
        return(sapply(ls(h), function(k) get(k, h, inherits=FALSE)))
    }

    sort_seq <- function(seq, len) {
        fs <- gen_freq(seq, len)
        seqs <- names(fs)
        inds <- order(-fs, seqs)
#    cat(paste.(seqs[inds], 100 * fs[inds] / sum(fs), collapse="\n", digits=3),
        cat(paste(seqs[inds], 100 * fs[inds] / sum(fs), collapse="\n"),
            "\n")
    }

    find_seq <- function(seq, s) {
        freqs <- gen_freq(seq, nchar(s))
        if (s %in% names(freqs))
            return(freqs[[s]])
        return(0L)
    }

    knucleotide <- function(args) {
        in_filename = args[[1]]
        f <- file(in_filename, "r")
        print(f)
        while (length(line <- readLines(f, n=1, warn=FALSE))) {
            first_char <- substr(line, 1L, 1L)
            if (first_char == '>' || first_char == ';')
                if (substr(line, 2L, 3L) == 'TH')
                    break
        }

        n <- 0L
        cap <- 8L
        str_buf <- character(cap)
        while (length(line <- scan(f, what="", nmax=1, quiet=TRUE))) {
            first_char <- substr(line, 1L, 1L)
            if (first_char == '>' || first_char == ';')
                break
            n <- n + 1L
            # ensure O(N) resizing (instead of O(N^2))
            str_buf[[cap <- if (cap < n) 2L * cap else cap]] <- ""
            str_buf[[n]] <- line
        }
        length(str_buf) <- n
        close(f)
        seq <- strsplit(paste(str_buf, collapse=""), split="")[[1]]

        for (frame in 1:2)
            sort_seq(seq, frame)
        for (s in c("GGT", "GGTA", "GGTATT", "GGTATTTTAATT", "GGTATTTTAATTTATAGT"))
            cat(find_seq(seq, tolower(s)), sep="\t", s, "\n")
    }

    paste. <- function (..., digits=16, sep=" ", collapse=NULL) {
        args <- list(...)
        if (length(args) == 0)
            if (length(collapse) == 0) character(0)
            else ""
        else {
            for(i in seq(along=args))
                if(is.numeric(args[[i]])) 
                    args[[i]] <- as.character(round(args[[i]], digits))
                else args[[i]] <- as.character(args[[i]])
            .Internal(paste(args, sep, collapse))
        }
    }

    file <- paste("../fasta/fasta", n, ".txt", sep="")
    knucleotide(file)
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
  capture.output(generator$execute(4000L), file = file.path(scratch, "fasta", paste0("fasta", 4000L, ".txt")))
  home <- setwd(file.path(scratch, "run"))
  on.exit({ setwd(home); unlink(scratch, recursive = TRUE) })
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(4000L))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("knucleotide %s %.4f\n", checksum, seconds))
})
