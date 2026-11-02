#@ weight 3
#@ param LIMIT int 5 90
#@ param RETRIES int 1 6
# Validated settings object for component @ID@.
#: @alias SettingsObject@ID@ {list{values: list[named: Any], get: fn(name: character) -> Any, set: fn(name: character, value: Any) -> Any}}

#: list{new: fn(...: Any) -> SettingsObject@ID@}
Settings@ID@ <- R6::R6Class( # ry: allow(object-system)
  "Settings@ID@",
  public = list(
    values = NULL,
    initialize = function(...) {
      defaults <- list(limit = @LIMIT@L, retries = @RETRIES@L, label = "component-@ID@")
      supplied <- rlang::list2(...)
      #: character[]
      unknown <- setdiff(names(supplied), names(defaults))
      if (length(unknown) > 0) {
        rlang::abort(
          paste0("Unknown setting: ", paste(unknown, collapse = ", ")),
          class = "settings_error"
        )
      }
      self$values <- utils::modifyList(defaults, supplied)
      private$validate()
    },
    get = function(name) {
      assertthat::assert_that(assertthat::is.string(name))
      value <- self$values[[name]]
      if (is.null(value)) {
        rlang::abort(paste0("No setting named ", name), class = "settings_error")
      }
      value
    },
    set = function(name, value) {
      self$values[[name]] <- value
      private$validate()
      invisible(self)
    }
  ),
  private = list(
    validate = function() {
      assertthat::assert_that(
        assertthat::is.count(self$values$limit),
        assertthat::is.count(self$values$retries),
        assertthat::is.string(self$values$label)
      )
    }
  )
)

#: fn(settings: SettingsObject@ID@) -> character
settings_summary_@ID@ <- function(settings) {
  stopifnot(inherits(settings, "Settings@ID@"))
  paste0(
    snakecase::to_snake_case(settings$get("label")), ":",
    settings$get("limit"), "/", settings$get("retries")
  )
}
#@ test
test_that("settings @ID@ validate and summarise", {
  #: SettingsObject@ID@
  settings <- Settings@ID@$new(limit = 3L)
  expect_equal(settings$get("limit"), 3L)
  expect_equal(settings_summary_@ID@(settings), "component_@ID@:3/@RETRIES@")
  expect_error(Settings@ID@$new(bogus = 1), class = "settings_error")
  expect_error(settings$set("limit", -1))
  expect_error(settings$get("missing"), class = "settings_error")
})
