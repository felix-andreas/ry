#@ weight 3
#@ param PATIENTS int 20 150
#@ param VISITS int 2 8
# Clinical trial visit schedules with date arithmetic.
enrol_@ID@ <- function(patients = @PATIENTS@L, start = as.Date("2023-03-01"), seed = 12L) {
  set.seed(seed)
  data.table::data.table(
    patient = sprintf("P%04d", seq_len(patients)),
    arm = sample(c("placebo", "active"), patients, replace = TRUE),
    enrolled = start + sample(0:90, patients, replace = TRUE)
  )
}

visit_schedule_@ID@ <- function(enrolment, visits = @VISITS@L, interval_days = 28L) {
  schedule <- enrolment[, .(visit = seq_len(visits)), by = .(patient, arm, enrolled)]
  schedule[, due := enrolled + (visit - 1L) * interval_days]
  schedule[, study_day := as.integer(due - enrolled) + 1L]
  schedule[]
}

visits_per_month_@ID@ <- function(schedule) {
  schedule[, .N, by = .(month = lubridate::floor_date(due, "month"))][order(month)]
}

randomise_@ID@ <- function(n, block = 4L, seed = 3L) {
  set.seed(seed)
  blocks <- ceiling(n / block)
  unlist(replicate(blocks, sample(rep(c("placebo", "active"), length.out = block)), simplify = FALSE))[seq_len(n)]
}

allocation_codes_@ID@ <- function(n) vapply(seq_len(n), function(i) substr(uuid::UUIDgenerate(use.time = FALSE), 1, 8), character(1))
#@ test
test_that("trial schedule @ID@ is complete", {
  enrolment <- enrol_@ID@()
  schedule <- visit_schedule_@ID@(enrolment)
  expect_equal(nrow(schedule), @PATIENTS@ * @VISITS@)
  expect_equal(min(schedule$study_day), 1L)
  expect_equal(sum(visits_per_month_@ID@(schedule)$N), nrow(schedule))
  arms <- randomise_@ID@(@PATIENTS@)
  expect_length(arms, @PATIENTS@)
  expect_lte(abs(sum(arms == "active") - sum(arms == "placebo")), 4L)
  expect_length(unique(allocation_codes_@ID@(10L)), 10L)
})
