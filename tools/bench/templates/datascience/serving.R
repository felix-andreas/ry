#@ weight 2
#@ param VERSION int 1 9
#@ ns exportClasses(ModelCard@ID@)
# Model serving: a plumber router, request validation and response formatting.
#: @type ModelCard@ID@ {list{name: character, coefficients: double[]}}

methods::setClass("ModelCard@ID@", representation(name = "character", coefficients = "numeric")) # ry: allow(object-system)

#: fn(card: ModelCard@ID@, x: double[]) -> double
predict_card_@ID@ <- function(card, x) {
  #: double[]
  coefficients <- card@coefficients
  stopifnot(length(x) == length(coefficients) - 1L)
  sum(coefficients * c(1, x))
}

#: fn(body: Any) -> double[]
validate_request_@ID@ <- function(body) {
  if (!is.list(body) || is.null(body$features)) {
    rlang::abort("features are required", class = "bad_request")
  }
  as.numeric(unlist(body$features))
}

#: fn(card: ModelCard@ID@, json: character) -> json
handle_request_@ID@ <- function(card, json) {
  body <- jsonlite::fromJSON(json, simplifyVector = FALSE)
  result <- tryCatch(
    list(status = 200L, prediction = predict_card_@ID@(card, validate_request_@ID@(body))),
    bad_request = function(e) list(status = 400L, error = conditionMessage(e))
  )
  jsonlite::toJSON(result, auto_unbox = TRUE)
}

#: fn(card: ModelCard@ID@) -> Plumber
build_router_@ID@ <- function(card) {
  router <- plumber::pr()
  #: fn(req: Any) -> json
  handler <- function(req) {
    handle_request_@ID@(card, req$postBody)
  }
  plumber::pr_post(router, "/v@VERSION@/predict", handler)
}
#@ test
test_that("serving @ID@ validates requests", {
  #: @new ModelCard@ID@
  card <- methods::new("ModelCard@ID@", name = "m", coefficients = c(1, 2, 3))
  ok <- jsonlite::fromJSON(handle_request_@ID@(card, '{"features": [1, 1]}'))
  expect_equal(ok$prediction, 6)
  bad <- jsonlite::fromJSON(handle_request_@ID@(card, '{"other": 1}'))
  expect_equal(bad$status, 400L)
  expect_s3_class(build_router_@ID@(card), "Plumber")
})
