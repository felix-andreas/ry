#@ weight 2
#@ param PATIENTS int 2 12
# Patient records exchanged as JSON documents and stored through an injectable backend.
patient_document_@ID@ <- function(id) {
  list(
    resourceType = "Patient",
    id = sprintf("pat-%03d", id),
    birthDate = format(as.Date("1960-01-01") + id * 400L),
    observations = lapply(1:2, function(i) list(code = c("bp", "hr")[[i]], value = 60 + id * i))
  )
}

parse_documents_@ID@ <- function(text) {
  parsed <- RcppSimdJson::fparse(text)
  data.frame(
    id = parsed$id,
    birth = as.Date(parsed$birthDate),
    age_2024 = lubridate::interval(as.Date(parsed$birthDate), as.Date("2024-01-01")) %/% lubridate::years(1)
  )
}

store_documents_@ID@ <- function(documents, container, backend = NULL) {
  if (is.null(backend)) {
    backend <- function(container, name, body) {
      endpoint <- AzureStor::blob_endpoint("https://store.blob.core.windows.net")
      AzureStor::upload_blob(AzureStor::blob_container(endpoint, container), textConnection(body), name)
    }
  }
  names <- vapply(documents, function(d) paste0(d$id, ".json"), character(1))
  for (i in seq_along(documents)) {
    backend(container, names[[i]], as.character(jsonlite::toJSON(documents[[i]], auto_unbox = TRUE)))
  }
  names
}
#@ test
test_that("records @ID@ round trip through JSON", {
  documents <- lapply(seq_len(@PATIENTS@), patient_document_@ID@)
  stored <- list()
  names <- store_documents_@ID@(documents, "container", backend = function(container, name, body) stored[[name]] <<- body)
  expect_length(names, @PATIENTS@)
  expect_equal(length(stored), @PATIENTS@)
  parsed <- parse_documents_@ID@(jsonlite::toJSON(documents, auto_unbox = TRUE))
  expect_equal(nrow(parsed), @PATIENTS@)
  expect_true(all(parsed$age_2024 > 50))
})
