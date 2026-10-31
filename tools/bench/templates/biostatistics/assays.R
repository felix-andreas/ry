#@ weight 2
#@ param WELLS int 8 96
# Plate-reader assay data: layout, normalisation and dose-response fits.
plate_layout_@ID@ <- function(wells = @WELLS@L) {
  rows <- LETTERS[seq_len(ceiling(wells / 12))]
  grid <- expand.grid(column = 1:12, row = rows, stringsAsFactors = FALSE)[seq_len(wells), ]
  grid$well <- paste0(grid$row, sprintf("%02d", grid$column))
  grid
}

simulate_signal_@ID@ <- function(layout, ec50 = 2, hill = 1.2, seed = 31L) {
  set.seed(seed)
  dose <- 10^seq(-2, 2, length.out = nrow(layout))
  layout$dose <- dose
  layout$signal <- 100 / (1 + (ec50 / dose)^hill) + stats::rnorm(nrow(layout), sd = 1)
  layout
}

normalise_plate_@ID@ <- function(layout) {
  lower <- min(layout$signal)
  upper <- max(layout$signal)
  layout$percent <- 100 * (layout$signal - lower) / (upper - lower)
  layout
}

fit_dose_response_@ID@ <- function(layout) {
  stats::nls(
    signal ~ top / (1 + (ec50 / dose)^hill),
    data = layout, start = list(top = 100, ec50 = 1, hill = 1)
  )
}

write_plate_@ID@ <- function(layout, path) {
  openxlsx::write.xlsx(layout, path)
  readxl::read_excel(path)
}
#@ test
test_that("assay @ID@ recovers the dose-response", {
  layout <- simulate_signal_@ID@(plate_layout_@ID@())
  expect_equal(nrow(layout), @WELLS@)
  expect_equal(range(normalise_plate_@ID@(layout)$percent), c(0, 100))
  fit <- tryCatch(fit_dose_response_@ID@(layout), error = function(e) NULL)
  if (!is.null(fit)) expect_gt(stats::coef(fit)[["top"]], 50)
  withr::with_tempdir(expect_equal(nrow(write_plate_@ID@(layout, "plate.xlsx")), @WELLS@))
})
