#@ weight 3
#@ param ROWS int 40 300
#@ param BINS int 3 8
# Feature engineering on plain data frames.
make_frame_@ID@ <- function(rows = @ROWS@L, seed = 2L) {
  set.seed(seed)
  data.frame(
    age = round(stats::runif(rows, 18, 80)),
    income = stats::rlnorm(rows, 10, 0.5),
    segment = factor(sample(c("a", "b", "c"), rows, replace = TRUE)),
    stringsAsFactors = FALSE
  )
}

standardise_@ID@ <- function(frame, columns = c("age", "income")) {
  frame[columns] <- lapply(frame[columns], function(x) as.numeric(scale(x)))
  frame
}

bin_column_@ID@ <- function(x, bins = @BINS@L) {
  cut(x, breaks = stats::quantile(x, seq(0, 1, length.out = bins + 1L)), include.lowest = TRUE, labels = FALSE)
}

one_hot_@ID@ <- function(frame, column) {
  levels <- sort(unique(as.character(frame[[column]])))
  encoded <- vapply(levels, function(level) as.integer(frame[[column]] == level), integer(nrow(frame)))
  colnames(encoded) <- paste0(column, "_", levels)
  cbind(frame[setdiff(names(frame), column)], encoded)
}

missingness_@ID@ <- function(frame) {
  purrr::map_dbl(frame, function(column) mean(is.na(column)))
}
#@ test
test_that("features @ID@ are well formed", {
  frame <- make_frame_@ID@()
  scaled <- standardise_@ID@(frame)
  expect_equal(mean(scaled$age), 0, tolerance = 1e-8)
  expect_equal(sd(scaled$income), 1, tolerance = 1e-8)
  bins <- bin_column_@ID@(frame$income)
  expect_true(all(bins %in% seq_len(@BINS@)))
  encoded <- one_hot_@ID@(frame, "segment")
  expect_equal(rowSums(encoded[grep("^segment_", names(encoded))]), rep(1, @ROWS@))
  expect_equal(unname(missingness_@ID@(frame)), c(0, 0, 0))
})
