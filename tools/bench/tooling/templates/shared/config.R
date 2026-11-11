#@ weight 3
#@ param LIMIT int 5 90
#@ param RETRIES int 1 6
# Validated settings object for component @ID@.
#@ typed
type Settings@ID@ = Record{limit: integer, retries: integer, label: character}

settings_@ID@ <- function(limit: integer = @LIMIT@L, retries: integer = @RETRIES@L, label: character = "component-@ID@") -> Settings@ID@ {
  if (limit < 1L || retries < 1L) {
    rlang::abort("limit and retries must be positive counts", class = "settings_error")
  }
  @Settings@ID@((limit = limit, retries = retries, label = label))
}

settings_summary_@ID@ <- function(settings: Settings@ID@) -> character {
  paste0(
    snakecase::to_snake_case(settings$label), ":",
    settings$limit, "/", settings$retries
  )
}
#@ untyped
alias SettingsObject@ID@ = list{values: list[named: Any], get: fn(name: character) -> Any, set: fn(name: character, value: Any) -> Any}

Settings@ID@: list{new: fn(...: Any) -> SettingsObject@ID@} <- R6::R6Class( # ry: allow(object-system)
  "Settings@ID@",
  public = list(
    values = NULL,
    initialize = function(...) {
      defaults <- list(limit = @LIMIT@L, retries = @RETRIES@L, label = "component-@ID@")
      supplied <- rlang::list2(...)
      unknown: character[] <- setdiff(names(supplied), names(defaults))
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

settings_summary_@ID@ <- function(settings: SettingsObject@ID@) -> character {
  stopifnot(inherits(settings, "Settings@ID@"))
  paste0(
    snakecase::to_snake_case(settings$get("label")), ":",
    settings$get("limit"), "/", settings$get("retries")
  )
}
#@ end
#@ test
#@ typed
test_that("settings @ID@ validate and summarise", {
  settings <- settings_@ID@(limit = 3L)
  expect_equal(settings$limit, 3L)
  expect_equal(settings_summary_@ID@(settings), "component_@ID@:3/@RETRIES@")
  expect_error({
    settings_@ID@(limit = -1L)
    NULL
  }, class = "settings_error")
  changed <- settings
  changed$limit <- 5L
  expect_equal(settings$limit, 3L)
  expect_equal(changed$limit, 5L)
})
#@ untyped
test_that("settings @ID@ validate and summarise", {
  settings: SettingsObject@ID@ <- Settings@ID@$new(limit = 3L)
  expect_equal(settings$get("limit"), 3L)
  expect_equal(settings_summary_@ID@(settings), "component_@ID@:3/@RETRIES@")
  expect_error(Settings@ID@$new(bogus = 1), class = "settings_error")
  expect_error(settings$set("limit", -1))
  expect_error(settings$get("missing"), class = "settings_error")
})
#@ end
