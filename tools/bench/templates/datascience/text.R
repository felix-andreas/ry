#@ weight 2
#@ param DOCS int 3 20
# Bag-of-words text features.
corpus_@ID@ <- function(docs = @DOCS@L) {
  vocabulary <- c("data", "model", "table", "query", "cache", "index", "frame", "vector")
  vapply(seq_len(docs), function(i) {
    paste(vocabulary[((i + 0:4) %% length(vocabulary)) + 1L], collapse = " ")
  }, character(1))
}

term_matrix_@ID@ <- function(docs) {
  tokens <- stringr::str_split(docs, "\\s+")
  vocabulary <- sort(unique(unlist(tokens)))
  counts <- t(vapply(tokens, function(words) table(factor(words, levels = vocabulary)), numeric(length(vocabulary))))
  colnames(counts) <- vocabulary
  counts
}

tf_idf_@ID@ <- function(counts) {
  tf <- counts / pmax(rowSums(counts), 1)
  idf <- log(nrow(counts) / pmax(colSums(counts > 0), 1))
  sweep(tf, 2, idf, `*`)
}

similarity_@ID@ <- function(matrix) {
  normalised <- matrix / pmax(sqrt(rowSums(matrix^2)), 1e-12)
  normalised %*% t(normalised)
}
#@ test
test_that("text features @ID@ are consistent", {
  docs <- corpus_@ID@()
  counts <- term_matrix_@ID@(docs)
  expect_equal(nrow(counts), @DOCS@)
  expect_equal(unname(rowSums(counts)), rep(5, @DOCS@))
  similarity <- similarity_@ID@(tf_idf_@ID@(counts))
  expect_equal(unname(diag(similarity)), rep(1, @DOCS@), tolerance = 1e-8)
})
