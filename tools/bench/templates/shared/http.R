#@ weight 2
#@ param PAGE int 1 50
# Request construction only; sending is left to injectable transport functions.
build_request_@ID@ <- function(base_url, resource, query = list()) {
  httr2::request(base_url) |>
    httr2::req_url_path_append(resource) |>
    httr2::req_url_query(!!!query) |>
    httr2::req_headers(Accept = "application/json") |>
    httr2::req_user_agent("bench/@ID@")
}

legacy_url_@ID@ <- function(base_url, resource, page = @PAGE@L) {
  parts <- httr::parse_url(base_url)
  parts$path <- paste0(resource, "/", page)
  httr::build_url(parts)
}

fetch_json_@ID@ <- function(request, transport = function(req) httr2::req_perform(req)) {
  response <- transport(request)
  httr2::resp_body_json(response)
}

service_@ID@ <- function() {
  router <- plumber::pr()
  router <- plumber::pr_get(router, "/health@ID@", function() list(status = "ok"))
  plumber::pr_post(router, "/echo@ID@", function(req) list(length = length(req$args)))
}
#@ test
test_that("requests @ID@ are built as expected", {
  request <- build_request_@ID@("https://example.test/api", "items", list(page = @PAGE@))
  expect_match(request$url, "^https://example.test/api/items\\?page=@PAGE@$")
  expect_equal(request$headers$Accept, "application/json")
  expect_equal(legacy_url_@ID@("https://example.test", "items"), "https://example.test/items/@PAGE@")
  fake <- function(req) httr2::response_json(body = list(page = @PAGE@L))
  expect_equal(fetch_json_@ID@(request, fake)$page, @PAGE@L)
  router <- service_@ID@()
  expect_s3_class(router, "Plumber")
})
