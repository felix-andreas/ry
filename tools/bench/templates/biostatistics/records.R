#@ weight 2
#@ param PATIENTS int 2 12
# Patient records exchanged as JSON documents and stored through an injectable backend.
alias Patient@ID@ = list{resourceType: character, id: character, birthDate: character, observations: list[list{code: character, value: double}]}

patient_document_@ID@ <- function(id: integer) -> Patient@ID@ {
  list(
    resourceType = "Patient",
    id = sprintf("pat-%03d", id),
    birthDate = format(as.Date("1960-01-01") + id * 400L),
    observations = lapply(1:2, function(i: integer) -> list{code: character, value: double} list(code = c("bp", "hr")[[i]], value = 60 + id * i))
  )
}

parse_documents_@ID@ <- function(text: json | character) -> data.frame {
  parsed <- RcppSimdJson::fparse(text)
  data.frame(
    id = parsed$id,
    birth = as.Date(parsed$birthDate),
    age_2024 = as.numeric(difftime(as.Date("2024-01-01"), as.Date(parsed$birthDate), units = "days")) / 365.25
  )
}

store_documents_@ID@ <- function(documents: list[Patient@ID@], container: character, backend: (fn(container: character, name: character, body: character) -> character | NULL) | NULL = NULL) -> character[] {
  if (is.null(backend)) {
    backend <- function(container: character, name: character, body: character) -> NULL {
      endpoint <- AzureStor::blob_endpoint("https://store.blob.core.windows.net")
      AzureStor::upload_blob(AzureStor::blob_container(endpoint, container), textConnection(body), name)
    }
  }
  names <- vapply(documents, function(d: Patient@ID@) -> character paste0(d$id, ".json"), character(1))
  for (i in seq_along(documents)) {
    backend(container, names[[i]], as.character(jsonlite::toJSON(documents[[i]], auto_unbox = TRUE)))
  }
  names
}
#@ test
test_that("records @ID@ round trip through JSON", {
  documents <- lapply(seq_len(@PATIENTS@), patient_document_@ID@)
  stored <- list()
  names <- store_documents_@ID@(documents, "container", backend = function(container: character, name: character, body: character) -> character stored[[name]] <<- body)
  expect_length(names, @PATIENTS@)
  expect_equal(length(stored), @PATIENTS@)
  parsed <- parse_documents_@ID@(jsonlite::toJSON(documents, auto_unbox = TRUE))
  expect_equal(nrow(parsed), @PATIENTS@)
  ages: @if-unknown double[] <- parsed$age_2024
  expect_true(all(ages > 50))
})
