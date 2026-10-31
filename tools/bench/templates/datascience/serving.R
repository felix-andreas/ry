#@ weight 2
#@ param VERSION int 1 9
#@ ns exportClasses(ModelCard@ID@)
# Model serving: a plumber router, request validation and response formatting.
methods::setClass("ModelCard@ID@", representation(name = "character", coefficients = "numeric"))

predict_card_@ID@ <- function(card, x) {
  stopifnot(length(x) == length(card@coefficients) - 1L)
  sum(card@coefficients * c(1, x))
}

validate_request_@ID@ <- function(body) {
  if (!is.list(body) || is.null(body$features)) {
    rlang::abort("features are required", class = "bad_request")
  }
  as.numeric(unlist(body$features))
}

handle_request_@ID@ <- function(card, json) {
  body <- jsonlite::fromJSON(json, simplifyVector = FALSE)
  result <- tryCatch(
    list(status = 200L, prediction = predict_card_@ID@(card, validate_request_@ID@(body))),
    bad_request = function(e) list(status = 400L, error = conditionMessage(e))
  )
  jsonlite::toJSON(result, auto_unbox = TRUE)
}

build_router_@ID@ <- function(card) {
  router <- plumber::pr()
  plumber::pr_post(router, "/v@VERSION@/predict", function(req) {
    handle_request_@ID@(card, req$postBody)
  })
}
#@ test
test_that("serving @ID@ validates requests", {
  card <- methods::new("ModelCard@ID@", name = "m", coefficients = c(1, 2, 3))
  ok <- jsonlite::fromJSON(handle_request_@ID@(card, '{"features": [1, 1]}'))
  expect_equal(ok$prediction, 6)
  bad <- jsonlite::fromJSON(handle_request_@ID@(card, '{"other": 1}'))
  expect_equal(bad$status, 400L)
  expect_s3_class(build_router_@ID@(card), "Plumber")
})
