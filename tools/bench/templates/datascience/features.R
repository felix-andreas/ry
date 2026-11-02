#@ weight 3
#@ param ROWS int 40 300
#@ param BINS int 3 8
# Feature engineering on plain data frames.
#: fn([rows]: integer, [seed]: integer) -> data.frame
make_frame_@ID@ <- function(rows = @ROWS@L, seed = 2L) {
  set.seed(seed)
  data.frame(
    age = round(stats::runif(rows, 18, 80)),
    income = stats::rlnorm(rows, 10, 0.5),
    segment = factor(sample(c("a", "b", "c"), rows, replace = TRUE)),
    stringsAsFactors = FALSE
  )
}

#: fn(frame: data.frame, [columns]: character[]) -> data.frame
standardise_@ID@ <- function(frame, columns = c("age", "income")) {
  frame[columns] <- lapply(frame[columns], function(x) as.numeric(scale(x)))
  frame
}

#: fn(x: double[], [bins]: integer) -> integer[]
bin_column_@ID@ <- function(x, bins = @BINS@L) {
  cut(x, breaks = stats::quantile(x, seq(0, 1, length.out = bins + 1L)), include.lowest = TRUE, labels = FALSE)
}

#: fn(frame: data.frame, column: character) -> data.frame
one_hot_@ID@ <- function(frame, column) {
  #: factor | character[]
  values <- frame[[column]]
  labels <- as.character(values)
  levels <- sort(unique(labels))
  encoded <- vapply(levels, function(level) as.integer(labels == level), integer(nrow(frame)))
  colnames(encoded) <- paste0(column, "_", levels)
  cbind(frame[setdiff(names(frame), column)], encoded)
}

#: fn(frame: data.frame) -> double[named]
missingness_@ID@ <- function(frame) {
  purrr::map_dbl(frame, function(column) mean(is.na(column)))
}
#@ test
test_that("features @ID@ are well formed", {
  frame <- make_frame_@ID@()
  scaled <- standardise_@ID@(frame)
  #: double[]
  scaled_age <- scaled$age
  #: double[]
  scaled_income <- scaled$income
  expect_equal(mean(scaled_age), 0, tolerance = 1e-8)
  expect_equal(sd(scaled_income), 1, tolerance = 1e-8)
  #: double[]
  income <- frame$income
  bins <- bin_column_@ID@(income)
  expect_true(all(bins %in% seq_len(@BINS@)))
  encoded <- one_hot_@ID@(frame, "segment")
  #: data.frame
  indicators <- encoded[grep("^segment_", names(encoded))]
  expect_equal(rowSums(indicators), rep(1, @ROWS@))
  expect_equal(unname(missingness_@ID@(frame)), c(0, 0, 0))
})
