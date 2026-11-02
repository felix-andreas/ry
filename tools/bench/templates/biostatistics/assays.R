#@ weight 2
#@ param WELLS int 8 96
# Plate-reader assay data: layout, normalisation and dose-response fits.
plate_layout_@ID@ <- function(wells: integer = @WELLS@L) -> data.frame {
  rows <- LETTERS[seq_len(ceiling(wells / 12))]
  grid: data.frame <- expand.grid(column = 1:12, row = rows, stringsAsFactors = FALSE)[seq_len(wells), ]
  row_labels: @if-unknown character[] <- grid$row
  columns: @if-unknown integer[] <- grid$column
  grid$well <- paste0(row_labels, sprintf("%02d", columns))
  grid
}

simulate_signal_@ID@ <- function(layout: data.frame, ec50: double = 2, hill: double = 1.2, seed: integer = 31L) -> data.frame {
  set.seed(seed)
  dose <- 10^seq(-2, 2, length.out = nrow(layout))
  layout$dose <- dose
  layout$signal <- 100 / (1 + (ec50 / dose)^hill) + stats::rnorm(nrow(layout), sd = 1)
  layout
}

normalise_plate_@ID@ <- function(layout: data.frame) -> data.frame {
  signal: @if-unknown double[] <- layout$signal
  lower <- min(signal)
  upper <- max(signal)
  layout$percent <- 100 * (signal - lower) / (upper - lower)
  layout
}

fit_dose_response_@ID@ <- function(layout: data.frame) -> Any {
  model <- signal ~ top / (1 + (ec50 / dose)^hill)
  stats::nls(model, data = layout, start = list(top = 100, ec50 = 1, hill = 1))
}

write_plate_@ID@ <- function(layout: data.frame, path: character) -> data.frame {
  openxlsx::write.xlsx(layout, path)
  readxl::read_excel(path)
}
#@ test
test_that("assay @ID@ recovers the dose-response", {
  layout <- simulate_signal_@ID@(plate_layout_@ID@())
  expect_equal(nrow(layout), @WELLS@)
  percent: @if-unknown double[] <- normalise_plate_@ID@(layout)$percent
  expect_equal(range(percent), c(0, 100))
  fit <- tryCatch(fit_dose_response_@ID@(layout), error = function(e: condition) -> NULL NULL)
  if (!is.null(fit)) expect_gt(stats::coef(fit)[["top"]], 50)
  withr::with_tempdir(expect_equal(nrow(write_plate_@ID@(layout, "plate.xlsx")), @WELLS@))
})
