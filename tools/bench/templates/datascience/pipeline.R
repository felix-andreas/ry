#@ weight 3
#@ param STEPS int 2 6
# A pipeline of named steps with timing and a content-addressed cache.
#: @type Pipeline@ID@ {list{steps: list[named: fn(x: Any) -> Any], history: character[], add: fn(name: character, fn: fn(x: Any) -> Any) -> Pipeline@ID@, run: fn(input: Any) -> Any}}

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
        #: Any
        value <- self$steps[[name]](value)
        self$history <- c(self$history, sprintf("%s:%s", name, digest::digest(value, algo = "crc32")))
      }
      value
    }
  )
)

#: fn([steps]: integer) -> Pipeline@ID@
default_pipeline_@ID@ <- function(steps = @STEPS@L) {
  #: @new Pipeline@ID@
  pipeline <- Pipeline@ID@$new()
  for (i in seq_len(steps)) {
    local({
      shift <- i
      pipeline$add(paste0("step", shift), function(x) x + shift)
    })
  }
  pipeline
}
#@ test
test_that("pipeline @ID@ applies its steps in order", {
  pipeline <- default_pipeline_@ID@()
  expect_equal(pipeline$run(0), sum(seq_len(@STEPS@)))
  expect_length(pipeline$history, @STEPS@)
  expect_match(pipeline$history[[1]], "^step1:")
})
