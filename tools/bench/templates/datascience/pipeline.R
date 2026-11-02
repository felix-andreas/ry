#@ weight 3
#@ param STEPS int 2 6
# A pipeline of named steps with timing and a content-addressed cache.
#@ typed
type Pipeline@ID@ = Record{steps: list[named: fn(x: double) -> double]}

pipeline_add_@ID@ <- function(pipeline: Pipeline@ID@, name: character, step: fn(x: double) -> double) -> Pipeline@ID@ {
  steps <- pipeline$steps
  steps[[name]] <- step
  @Pipeline@ID@((steps = steps,))
}

pipeline_run_@ID@ <- function(pipeline: Pipeline@ID@, input: double) -> Record{value: double, history: character[]} {
  value <- input
  history <- character()
  for (name in names(pipeline$steps)) {
    step <- pipeline$steps[[name]]
    value <- step(value)
    history <- c(history, sprintf("%s:%s", name, digest::digest(value, algo = "crc32")))
  }
  (value = value, history = history)
}

default_pipeline_@ID@ <- function(steps: integer = @STEPS@L) -> Pipeline@ID@ {
  pipeline <- @Pipeline@ID@((steps = list(),))
  for (i in seq_len(steps)) {
    shift <- i
    pipeline <- pipeline_add_@ID@(pipeline, paste0("step", shift), local({
      by <- shift
      function(x: double) -> double x + by
    }))
  }
  pipeline
}
#@ untyped
type Pipeline@ID@ = list{steps: list[named: fn(x: Any) -> Any], history: character[], add: fn(name: character, fn: fn(x: Any) -> Any) -> Pipeline@ID@, run: fn(input: Any) -> Any}

Pipeline@ID@ <- R6::R6Class( # ry: allow(object-system)
  "Pipeline@ID@",
  public = list(
    steps = list(),
    history = NULL,
    initialize = function() self$history <- character(),
    add = function(name, fn) {
      assertthat::assert_that(is.function(fn), assertthat::is.string(name))
      self$steps[[name]] <- fn
      invisible(self)
    },
    run = function(input) {
      value <- input
      for (name in names(self$steps)) {
        value: Any <- self$steps[[name]](value)
        self$history <- c(self$history, sprintf("%s:%s", name, digest::digest(value, algo = "crc32")))
      }
      value
    }
  )
)

default_pipeline_@ID@ <- function(steps: integer = @STEPS@L) -> Pipeline@ID@ {
  pipeline <- @Pipeline@ID@(Pipeline@ID@$new())
  for (i in seq_len(steps)) {
    local({
      shift <- i
      pipeline$add(paste0("step", shift), function(x) x + shift)
    })
  }
  pipeline
}
#@ end
#@ test
#@ typed
test_that("pipeline @ID@ applies its steps in order", {
  pipeline <- default_pipeline_@ID@()
  outcome <- pipeline_run_@ID@(pipeline, 0)
  expect_equal(outcome$value, sum(seq_len(@STEPS@)))
  expect_length(outcome$history, @STEPS@)
  expect_match(outcome$history[[1]], "^step1:")
})
#@ untyped
test_that("pipeline @ID@ applies its steps in order", {
  pipeline <- default_pipeline_@ID@()
  expect_equal(pipeline$run(0), sum(seq_len(@STEPS@)))
  expect_length(pipeline$history, @STEPS@)
  expect_match(pipeline$history[[1]], "^step1:")
})
#@ end
