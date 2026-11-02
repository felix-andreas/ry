#@ weight 3
#@ param PATIENTS int 20 150
#@ param VISITS int 2 8
# Clinical trial visit schedules with date arithmetic.
#:= #: @alias Enrolment@ID@ {DataFrame{patient: character[], arm: character[], enrolled: Date}}
#:= #: @alias Schedule@ID@ {DataFrame{patient: character[], arm: character[], enrolled: Date, visit: integer[], due: Date, study_day: integer[]}}
#: @alias Enrolment@ID@ {data.table{patient: character[], arm: character[], enrolled: Date}}
#: @alias Schedule@ID@ {data.table{patient: character[], arm: character[], enrolled: Date, visit: integer[], due: Date, study_day: integer[]}}

#: fn([patients]: integer, [start]: Date, [seed]: integer) -> Enrolment@ID@
enrol_@ID@ <- function(patients = @PATIENTS@L, start = as.Date("2023-03-01"), seed = 12L) {
  set.seed(seed)
  #:= DataFrame(
  data.table::data.table(
    patient = sprintf("P%04d", seq_len(patients)),
    arm = sample(c("placebo", "active"), patients, replace = TRUE),
    enrolled = start + sample(0:90, patients, replace = TRUE)
  )
}

#: fn(enrolment: Enrolment@ID@, [visits]: integer, [interval_days]: integer) -> Schedule@ID@
visit_schedule_@ID@ <- function(enrolment, visits = @VISITS@L, interval_days = 28L) {
  #:= schedule <- DataFrame.join(enrolment, DataFrame(visit = seq_len(visits)), how = "cross")
  #:= schedule <- DataFrame.with_columns(schedule, due = col("enrolled") + (col("visit") - 1L) * interval_days)
  #:= schedule <- DataFrame.with_columns(schedule, study_day = col("due") - col("enrolled") + 1L)
  #:= schedule
  schedule <- enrolment[, .(visit = seq_len(visits)), by = .(patient, arm, enrolled)]
  schedule[, due := enrolled + (visit - 1L) * interval_days]
  schedule[, study_day := as.integer(due - enrolled) + 1L]
  schedule[]
}

#:= #: fn(schedule: Schedule@ID@) -> DataFrame{month: Date, N: integer[]}
#: fn(schedule: Schedule@ID@) -> data.table{month: Date, N: integer[]}
visits_per_month_@ID@ <- function(schedule) {
  #:= DataFrame.sort(GroupBy.agg(DataFrame.group_by(DataFrame(month = lubridate::floor_date(schedule$due, "month")), "month"), N = Expr.len()), "month")
  schedule[, .N, by = .(month = lubridate::floor_date(due, "month"))][order(month)]
}

#: fn(n: integer, [block]: integer, [seed]: integer) -> character[]
randomise_@ID@ <- function(n, block = 4L, seed = 3L) {
  set.seed(seed)
  blocks <- ceiling(n / block)
  unlist(replicate(blocks, sample(rep(c("placebo", "active"), length.out = block)), simplify = FALSE))[seq_len(n)]
}

#: fn(n: integer) -> character[]
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
