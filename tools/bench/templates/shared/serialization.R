#@ weight 3
#@ param ROWS int 2 15
# Round trips between R lists, JSON text and content hashes.
make_record_@ID@ <- function(id) {
  list(
    id = id,
    key = uuid::UUIDgenerate(use.time = FALSE),
    tags = c("a", "b", "c")[seq_len(1 + id %% 3)],
    score = id / 7
  )
}

records_to_json_@ID@ <- function(records) {
  jsonlite::toJSON(records, auto_unbox = TRUE, digits = NA)
}

json_to_records_@ID@ <- function(text) {
  jsonlite::fromJSON(text, simplifyVector = FALSE)
}

fast_parse_@ID@ <- function(text) {
  RcppSimdJson::fparse(as.character(text))
}

fingerprint_@ID@ <- function(records) {
  digest::digest(records_to_json_@ID@(records), algo = "sha256", serialize = FALSE)
}
#@ test
test_that("serialization @ID@ round trips", {
  records <- lapply(seq_len(@ROWS@), make_record_@ID@)
  text <- records_to_json_@ID@(records)
  back <- json_to_records_@ID@(text)
  expect_length(back, @ROWS@)
  expect_equal(back[[1]]$id, 1L)
  expect_equal(vapply(back, function(r) r$score, numeric(1)), seq_len(@ROWS@) / 7)
  fast <- fast_parse_@ID@(text)
  expect_equal(length(fast), @ROWS@)
  expect_equal(nchar(fingerprint_@ID@(records)), 64L)
  expect_equal(fingerprint_@ID@(records), fingerprint_@ID@(records))
  expect_match(make_record_@ID@(1L)$key, "^[0-9a-f-]{36}$")
})
