#@ weight 3
#@ param LENGTH int 20 200
#@ param KMER int 2 5
# Nucleotide sequence utilities.
#: fn([length]: integer, [seed]: integer) -> character
random_sequence_@ID@ <- function(length = @LENGTH@L, seed = 15L) {
  set.seed(seed)
  paste(sample(c("A", "C", "G", "T"), length, replace = TRUE), collapse = "")
}

#: fn(sequence: character) -> character
reverse_complement_@ID@ <- function(sequence) {
  complement <- chartr("ACGT", "TGCA", sequence)
  paste(rev(strsplit(complement, "")[[1]]), collapse = "")
}

#: fn(sequence: character) -> double[]
gc_content_@ID@ <- function(sequence) {
  stringr::str_count(sequence, "[GC]") / stringr::str_length(sequence)
}

#: fn(sequence: character, [k]: integer) -> integer[]
kmer_counts_@ID@ <- function(sequence, k = @KMER@L) {
  starts <- seq_len(nchar(sequence) - k + 1L)
  kmers <- stringr::str_sub(sequence, starts, starts + k - 1L)
  sort(table(kmers), decreasing = TRUE)
}

#: fn(sequence: character, motif: character) -> integer[]
find_motif_@ID@ <- function(sequence, motif) {
  matches <- gregexpr(paste0("(?=", motif, ")"), sequence, perl = TRUE)[[1]]
  if (identical(as.integer(matches), -1L)) integer(0) else as.integer(matches)
}
#@ test
test_that("sequence utilities @ID@ are self-inverse", {
  sequence <- random_sequence_@ID@()
  expect_equal(nchar(sequence), @LENGTH@)
  expect_equal(reverse_complement_@ID@(reverse_complement_@ID@(sequence)), sequence)
  expect_gte(gc_content_@ID@(sequence), 0)
  expect_equal(sum(kmer_counts_@ID@(sequence)), @LENGTH@ - @KMER@ + 1L)
  expect_equal(find_motif_@ID@("ACGTACGT", "ACG"), c(1L, 5L))
})
