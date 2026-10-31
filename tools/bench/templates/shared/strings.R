#@ weight 3
#@ param WORDS int 2 9
# Name normalisation and small string utilities.
normalise_names_@ID@ <- function(x) {
  x |>
    stringr::str_trim() |>
    stringr::str_replace_all("[^[:alnum:] ]", " ") |>
    snakecase::to_snake_case()
}

title_@ID@ <- function(x) tools::toTitleCase(tolower(x))

tokens_@ID@ <- function(text, min_length = 2L) {
  words <- stringr::str_split(stringr::str_to_lower(text), "[^a-z0-9]+")[[1]]
  purrr::keep(words, function(word) nchar(word) >= min_length)
}

word_counts_@ID@ <- function(text) {
  counts <- table(tokens_@ID@(text))
  counts[order(-counts, names(counts))]
}

safe_ratio_@ID@ <- function(x, y) {
  safe <- purrr::possibly(function(a, b) {
    if (b == 0) rlang::abort("zero denominator")
    a / b
  }, otherwise = NA_real_)
  purrr::map2_dbl(x, y, safe)
}
#@ test
test_that("string helpers @ID@ behave", {
  expect_equal(normalise_names_@ID@(c(" Gross Margin! ", "Net-Income")), c("gross_margin", "net_income"))
  expect_equal(title_@ID@("the QUICK brown fox"), "The Quick Brown Fox")
  text <- paste(rep("alpha beta alpha gamma", @WORDS@), collapse = " ")
  expect_equal(unname(word_counts_@ID@(text)[["alpha"]]), 2L * @WORDS@)
  expect_equal(safe_ratio_@ID@(c(1, 2), c(2, 0)), c(0.5, NA))
})
