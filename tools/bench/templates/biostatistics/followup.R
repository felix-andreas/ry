#@ weight 2
#@ param PATIENTS int 200 700
#@ param SEVERE float 2.0 3.0 2
# Patient follow-up folded into person-time, an incidence rate and counts of severe events and
# withdrawals. The typed program models a follow-up outcome as a tag, an event's onset as a
# tuple and the summary as a record; the untyped one uses lists with a `kind` field.
alias Outcome@ID@ = :Event(days: double, severity: double) | :Censored(days: double) | :Withdrawn(days: double, visits: integer)

followup_@ID@ <- function(count: integer = @PATIENTS@L, seed: integer = 7L) -> list[Outcome@ID@] {
  set.seed(seed)
  days: double[] <- round(stats::rexp(count, 1 / 365), 1)
  severity: double[] <- stats::rgamma(count, shape = 2, rate = 1)
  draw: double[] <- stats::runif(count)
  out: list[Outcome@ID@] <- vector("list", count)
  for (i in seq_len(count)) {
#@ typed
    out[[i]] <- if (draw[[i]] < 0.35) :Event(days = days[[i]], severity = severity[[i]]) else if (draw[[i]] < 0.9) :Censored(days = days[[i]]) else :Withdrawn(days = days[[i]], visits = as.integer(days[[i]] %/% 90))
#@ untyped
    out[[i]] <- if (draw[[i]] < 0.35) list(kind = "event", days = days[[i]], severity = severity[[i]]) else if (draw[[i]] < 0.9) list(kind = "censored", days = days[[i]]) else list(kind = "withdrawn", days = days[[i]], visits = as.integer(days[[i]] %/% 90))
#@ end
  }
  out
}

incidence_@ID@ <- function(outcomes: list[Outcome@ID@], severe: double = @SEVERE@) -> Record{person_years: double, rate: double, severe: integer, withdrawn: integer, visits: integer} {
  person_days <- 0
  events <- 0L
  severe_events <- 0L
  withdrawn <- 0L
  visits <- 0L
  for (outcome in outcomes) {
#@ typed
    onset <- match (outcome) {
      :Event(days = d, severity = s) if (s > severe) => (d, 2L)
      :Event(days = d, severity = s) => (d, 1L)
      :Censored(days = d) => (d, 0L)
      :Withdrawn(days = d, visits = v) => { visits <- visits + v; (d, -1L) }
    }
#@ untyped
    onset <- if (outcome$kind == "event" && outcome$severity > severe) c(outcome$days, 2L)
    else if (outcome$kind == "event") c(outcome$days, 1L)
    else if (outcome$kind == "censored") c(outcome$days, 0L)
    else {
      visits <- visits + outcome$visits; c(outcome$days, -1L)
    }
#@ end
    person_days <- person_days + onset[[1L]]
    status <- onset[[2L]]
    if (status > 0L) events <- events + 1L
    if (status == 2L) severe_events <- severe_events + 1L
    if (status < 0L) withdrawn <- withdrawn + 1L
  }
  person_years <- person_days / 365.25
#@ typed
  (person_years = person_years, rate = events / person_years, severe = severe_events, withdrawn = withdrawn, visits = visits)
#@ untyped
  list(person_years = person_years, rate = events / person_years, severe = severe_events, withdrawn = withdrawn, visits = visits)
#@ end
}

cohort_rate_@ID@ <- function(cohorts: integer, count: integer = @PATIENTS@L) -> double {
  total <- 0
  for (seed in seq_len(cohorts)) {
    total <- total + incidence_@ID@(followup_@ID@(count, seed))$rate
  }
  total / cohorts
}

#@ test
test_that("incidence @ID@ folds follow-up into person-time and event counts", {
#@ typed
  outcomes <- list(:Event(days = 365.25, severity = 5), :Event(days = 365.25, severity = 1), :Censored(days = 730.5), :Withdrawn(days = 365.25, visits = 4L))
#@ untyped
  outcomes <- list(list(kind = "event", days = 365.25, severity = 5), list(kind = "event", days = 365.25, severity = 1), list(kind = "censored", days = 730.5), list(kind = "withdrawn", days = 365.25, visits = 4L))
#@ end
  summary <- incidence_@ID@(outcomes, severe = 2)
  expect_equal(summary$person_years, 5)
  expect_equal(summary$rate, 0.4)
  expect_equal(summary$severe, 1L)
  expect_equal(summary$withdrawn, 1L)
  expect_equal(summary$visits, 4L)
  expect_true(is.finite(cohort_rate_@ID@(2L)))
})
