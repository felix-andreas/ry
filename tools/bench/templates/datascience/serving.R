#@ weight 2
#@ param VERSION int 1 9
#@ untyped
#@ ns exportClasses(ModelCard@ID@)
#@ end
# Model serving: a plumber router, request validation and response formatting.
#@ typed
type ModelCard@ID@ = Record{name: character, coefficients: double[]}

new_card_@ID@ <- function(name: character, coefficients: double[]) -> ModelCard@ID@ {
  @ModelCard@ID@((name = name, coefficients = coefficients))
}

predict_card_@ID@ <- function(card: ModelCard@ID@, x: double[]) -> double {
  coefficients <- card$coefficients
  stopifnot(length(x) == length(coefficients) - 1L)
  sum(coefficients * c(1, x))
}

validate_request_@ID@ <- function(body: Any) -> Result<double[], character> {
  if (!is.list(body) || is.null(body$features)) {
    :Err("features are required")
  } else {
    :Ok(as.numeric(unlist(body$features)))
  }
}

handle_request_@ID@ <- function(card: ModelCard@ID@, json: character) -> json {
  body <- jsonlite::fromJSON(json, simplifyVector = FALSE)
  result: list{status: integer, prediction: double} | list{status: integer, error: character} <- match (validate_request_@ID@(body)) {
    :Ok(x) => list(status = 200L, prediction = predict_card_@ID@(card, x))
    :Err(message) => list(status = 400L, error = message)
  }
  jsonlite::toJSON(result, auto_unbox = TRUE)
}
#@ untyped
type ModelCard@ID@ = list{name: character, coefficients: double[]}

methods::setClass("ModelCard@ID@", representation(name = "character", coefficients = "numeric")) # ry: allow(object-system)

new_card_@ID@ <- function(name: character, coefficients: double[]) -> ModelCard@ID@ {
  @ModelCard@ID@(methods::new("ModelCard@ID@", name = name, coefficients = coefficients))
}

predict_card_@ID@ <- function(card: ModelCard@ID@, x: double[]) -> double {
  coefficients: double[] <- card@coefficients
  stopifnot(length(x) == length(coefficients) - 1L)
  sum(coefficients * c(1, x))
}

validate_request_@ID@ <- function(body: Any) -> double[] {
  if (!is.list(body) || is.null(body$features)) {
    rlang::abort("features are required", class = "bad_request")
  }
  as.numeric(unlist(body$features))
}

handle_request_@ID@ <- function(card: ModelCard@ID@, json: character) -> json {
  body <- jsonlite::fromJSON(json, simplifyVector = FALSE)
  result <- tryCatch(
    list(status = 200L, prediction = predict_card_@ID@(card, validate_request_@ID@(body))),
    bad_request = function(e) list(status = 400L, error = conditionMessage(e))
  )
  jsonlite::toJSON(result, auto_unbox = TRUE)
}
#@ end

build_router_@ID@ <- function(card: ModelCard@ID@) -> Plumber {
  router <- plumber::pr()
  handler <- function(req: Any) -> json {
    handle_request_@ID@(card, req$postBody)
  }
  plumber::pr_post(router, "/v@VERSION@/predict", handler)
}
#@ test
test_that("serving @ID@ validates requests", {
  card <- new_card_@ID@("m", c(1, 2, 3))
  ok <- jsonlite::fromJSON(handle_request_@ID@(card, '{"features": [1, 1]}'))
  expect_equal(ok$prediction, 6)
  bad <- jsonlite::fromJSON(handle_request_@ID@(card, '{"other": 1}'))
  expect_equal(bad$status, 400L)
  expect_s3_class(build_router_@ID@(card), "Plumber")
})
