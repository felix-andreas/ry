#@ weight 3
#@ param ROWS int 3 25
# Tabular files in three formats, written to and read back from a scratch directory.
write_tables_@ID@ <- function(directory: character, rows: integer = @ROWS@) -> list{csv: character, xlsx: character, txt: character, table: data.frame} {
  fs::dir_create(directory)
  table: data.frame <- data.frame(
    id = seq_len(rows),
    name = paste0("row", seq_len(rows)),
    value = round(seq_len(rows) * 1.5, 2),
    stringsAsFactors = FALSE
  )
  csv <- fs::path(directory, "table@ID@.csv")
  xlsx <- fs::path(directory, "table@ID@.xlsx")
  txt <- fs::path(directory, "table@ID@.txt")
  readr::write_csv(table, csv)
  openxlsx::write.xlsx(table, xlsx)
  labels: character[] <- table$name
  amounts: double[] <- table$value
  brio::write_lines(paste(labels, amounts), txt)
  list(csv = csv, xlsx = xlsx, txt = txt, table = table)
}

read_tables_@ID@ <- function(paths: list{csv: character, xlsx: character, txt: character, table: data.frame}) -> list{csv: data.frame, xlsx: data.frame, txt: character[]} {
  list(
    csv = readr::read_csv(paths$csv, show_col_types = FALSE),
    xlsx = readxl::read_excel(paths$xlsx),
    txt = brio::read_lines(paths$txt)
  )
}

cache_directory_@ID@ <- function() -> character {
  fs::path(rappdirs::user_cache_dir("bench"), "component@ID@")
}
#@ test
test_that("tables @ID@ survive a round trip through three formats", {
  withr::with_tempdir({
    paths <- write_tables_@ID@(fs::path_abs("out"))
    back <- read_tables_@ID@(paths)
    expect_equal(nrow(back$csv), @ROWS@)
    read_names: character[] <- back$csv$name
    written_names: character[] <- paths$table$name
    expect_equal(read_names, written_names)
    read_values: double[] <- back$xlsx$value
    written_values: double[] <- paths$table$value
    expect_equal(as.numeric(read_values), written_values)
    expect_length(back$txt, @ROWS@)
    expect_equal(fs::path_ext(paths$xlsx), "xlsx")
  })
  expect_match(as.character(cache_directory_@ID@()), "component@ID@$")
})
