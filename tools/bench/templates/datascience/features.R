#@ weight 3
#@ param ROWS int 40 300
#@ param BINS int 3 8
# Feature engineering on plain data frames.
make_frame_@ID@ <- function(rows: integer = @ROWS@L, seed: integer = 2L) -> data.frame {
  set.seed(seed)
  data.frame(
    age = round(stats::runif(rows, 18, 80)),
    income = stats::rlnorm(rows, 10, 0.5),
    segment = factor(sample(c("a", "b", "c"), rows, replace = TRUE)),
    stringsAsFactors = FALSE
  )
}

standardise_@ID@ <- function(frame: data.frame, columns: character[] = c("age", "income")) -> data.frame {
  frame[columns] <- lapply(frame[columns], function(x: double[]) -> double[] as.numeric(scale(x)))
  frame
}

bin_column_@ID@ <- function(x: double[], bins: integer = @BINS@L) -> integer[] {
  cut(x, breaks = stats::quantile(x, seq(0, 1, length.out = bins + 1L)), include.lowest = TRUE, labels = FALSE)
}

one_hot_@ID@ <- function(frame: data.frame, column: character) -> data.frame {
  values: factor | character[] <- frame[[column]]
  labels <- as.character(values)
  levels <- sort(unique(labels))
  encoded <- vapply(levels, function(level: character) -> integer[] as.integer(labels == level), integer(nrow(frame)))
  colnames(encoded) <- paste0(column, "_", levels)
  cbind(frame[setdiff(names(frame), column)], encoded)
}

missingness_@ID@ <- function(frame: data.frame) -> double[named] {
  purrr::map_dbl(frame, function(column: Any) -> double mean(is.na(column)))
}
#@ test
test_that("features @ID@ are well formed", {
  frame <- make_frame_@ID@()
  scaled <- standardise_@ID@(frame)
  scaled_age: double[] <- scaled$age
  scaled_income: double[] <- scaled$income
  expect_equal(mean(scaled_age), 0, tolerance = 1e-8)
  expect_equal(sd(scaled_income), 1, tolerance = 1e-8)
  income: double[] <- frame$income
  bins <- bin_column_@ID@(income)
  expect_true(all(bins %in% seq_len(@BINS@)))
  encoded <- one_hot_@ID@(frame, "segment")
  indicators: data.frame <- encoded[grep("^segment_", names(encoded))]
  expect_equal(rowSums(indicators), rep(1, @ROWS@))
  expect_equal(unname(missingness_@ID@(frame)), c(0, 0, 0))
})
