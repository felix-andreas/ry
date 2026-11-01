#@ weight 3
#@ param ROWS int 3 25
# Tabular files in three formats, written to and read back from a scratch directory.
#: fn(directory: character, [rows]: integer) -> list{csv: character, xlsx: character, txt: character, table: data.frame}
write_tables_@ID@ <- function(directory, rows = @ROWS@) {
  fs::dir_create(directory)
  #: data.frame
  table <- data.frame(
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
  #: character[]
  labels <- table$name
  #: double[]
  amounts <- table$value
  brio::write_lines(paste(labels, amounts), txt)
  list(csv = csv, xlsx = xlsx, txt = txt, table = table)
}

#: fn(paths: list{csv: character, xlsx: character, txt: character, table: data.frame}) -> list{csv: data.frame, xlsx: data.frame, txt: character[]}
read_tables_@ID@ <- function(paths) {
  list(
    csv = readr::read_csv(paths$csv, show_col_types = FALSE),
    xlsx = readxl::read_excel(paths$xlsx),
    txt = brio::read_lines(paths$txt)
  )
}

#: fn() -> character
cache_directory_@ID@ <- function() {
  fs::path(rappdirs::user_cache_dir("bench"), "component@ID@")
}
#@ test
test_that("tables @ID@ survive a round trip through three formats", {
  withr::with_tempdir({
    paths <- write_tables_@ID@(fs::path_abs("out"))
    back <- read_tables_@ID@(paths)
    expect_equal(nrow(back$csv), @ROWS@)
    #: character[]
    read_names <- back$csv$name
    #: character[]
    written_names <- paths$table$name
    expect_equal(read_names, written_names)
    #: double[]
    read_values <- back$xlsx$value
    #: double[]
    written_values <- paths$table$value
    expect_equal(as.numeric(read_values), written_values)
    expect_length(back$txt, @ROWS@)
    expect_equal(fs::path_ext(paths$xlsx), "xlsx")
  })
  expect_match(as.character(cache_directory_@ID@()), "component@ID@$")
})
