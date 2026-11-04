# ------------------------------------------------------------------
# The Computer Language Shootout
# http://shootout.alioth.debian.org/
#
# Contributed by Leo Osvald
# ------------------------------------------------------------------

execute <- function(n=150000L) {
    codes <- c(
        "A", "C", "G", "T", "U", "M", "R", "W", "S", "Y", "K", "V", "H", "D", "B",
        "N")
    complements <- c(
        "T", "G", "C", "A", "A", "K", "Y", "W", "S", "R", "M", "B", "D", "H", "V",
        "N")
    comp_map <- NULL
    comp_map[codes] <- complements
    comp_map[tolower(codes)] <- complements

    reversecomplement <- function(args) {
        in_filename = args[[1]]
        f <- file(in_filename, "r")
        while (length(s <- readLines(f, n=1, warn=FALSE))) {
            codes <- strsplit(s, split="")[[1]]
            if (codes[[1]] == '>')
                cat(s, "\n", sep="")
            else {
                cat(paste(comp_map[codes], collapse=""), "\n", sep="")
            }
        }
        close(f)
    }

    file <- paste("../fasta/fasta", n, ".txt", sep="")
    reversecomplement(file)
}

# ---------------------------------------------------------------------------
# Driver added for ry's `just bench-real --public` (not part of the upstream
# file). It runs `execute` once at a fixed size and prints one line:
#   <name> <checksum of printed output and result> <seconds>
# Run it from this directory: upstream code reads sibling files by relative path.
local({
  # The program reads ../fasta/fasta<n>.txt, so write that input with this
  # suite's fasta.R into a scratch directory first, outside the timed region.
  generator <- new.env()
  sys.source("fasta.R", envir = generator)
  scratch <- tempfile("shootout-")
  dir.create(file.path(scratch, "fasta"), recursive = TRUE)
  dir.create(file.path(scratch, "run"))
  capture.output(generator$execute(80000L), file = file.path(scratch, "fasta", paste0("fasta", 80000L, ".txt")))
  home <- setwd(file.path(scratch, "run"))
  on.exit({ setwd(home); unlink(scratch, recursive = TRUE) })
  started <- proc.time()[[3]]
  output <- capture.output(value <- execute(80000L))
  seconds <- proc.time()[[3]] - started
  text <- c(output, capture.output(str(value, vec.len = 3, digits.d = 10, give.attr = FALSE)))
  bytes <- utf8ToInt(paste(text, collapse = "\n"))
  checksum <- sprintf("%d:%.0f", length(bytes), sum(as.numeric(bytes) * (seq_along(bytes) %% 251 + 1)))
  cat(sprintf("reversecomplement %s %.4f\n", checksum, seconds))
})
