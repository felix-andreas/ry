#@ weight 2
#@ param PAGE int 1 50
# Request construction only; sending is left to injectable transport functions.
build_request_@ID@ <- function(base_url: character, resource: character, query: list[named: Any] = list()) -> httr2_request {
  request <- httr2::request(base_url) |>
    httr2::req_url_path_append(resource)
  # `do.call()` splices the query, where `!!!` would leave the argument untyped.
  request: httr2_request <- do.call(httr2::req_url_query, c(list(request), query))
  request |>
    httr2::req_headers(Accept = "application/json") |>
    httr2::req_user_agent("bench/@ID@")
}

legacy_url_@ID@ <- function(base_url: character, resource: character, page: integer = @PAGE@L) -> character {
  parts <- httr::parse_url(base_url)
  parts$path <- paste0(resource, "/", page)
  httr::build_url(parts)
}

fetch_json_@ID@ <- function(request: httr2_request, transport: fn(req: httr2_request) -> httr2_response = function(req: httr2_request) -> httr2_response httr2::req_perform(req)) -> Any {
  response <- transport(request)
  httr2::resp_body_json(response)
}

service_@ID@ <- function() -> Plumber {
  router <- plumber::pr()
  router <- plumber::pr_get(router, "/health@ID@", function() -> list{status: character} list(status = "ok"))
  echo <- function(req: environment) -> list{length: integer} {
    args: @if-unknown list[named: Any] <- req$args
    list(length = length(args))
  }
  plumber::pr_post(router, "/echo@ID@", echo)
}
#@ test
test_that("requests @ID@ are built as expected", {
  request <- build_request_@ID@("https://example.test/api", "items", list(page = @PAGE@))
  url: @if-unknown character <- request$url
  expect_match(url, "^https://example.test/api/items\\?page=@PAGE@$")
  headers: @if-unknown list[named: Any] <- request$headers
  expect_equal(headers$Accept, "application/json")
  expect_equal(legacy_url_@ID@("https://example.test", "items"), "https://example.test/items/@PAGE@")
  fake <- function(req: httr2_request) -> httr2_response httr2::response_json(body = list(page = @PAGE@L))
  expect_equal(fetch_json_@ID@(request, fake)$page, @PAGE@L)
  router <- service_@ID@()
  expect_s3_class(router, "Plumber")
})
