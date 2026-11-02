#@ weight 1
#@ param ROWS int 3 20
# Workbook and PDF reports for a position book.
#: fn([rows]: integer) -> data.frame
position_book_@ID@ <- function(rows = @ROWS@L) {
  #: data.frame
  data.frame(
    instrument = sprintf("BOND%03d", seq_len(rows)),
    notional = seq_len(rows) * 1e5,
    price = 99 + (seq_len(rows) %% 5) / 10
  )
}

#: fn(book: data.frame, path: character) -> character
write_workbook_@ID@ <- function(book, path) {
  workbook <- openxlsx::createWorkbook()
  openxlsx::addWorksheet(workbook, "positions")
  openxlsx::writeData(workbook, "positions", book)
  openxlsx::addStyle(workbook, "positions", openxlsx::createStyle(numFmt = "#,##0"), rows = seq_len(nrow(book)) + 1L, cols = 2L, gridExpand = TRUE)
  openxlsx::saveWorkbook(workbook, path, overwrite = TRUE)
  invisible(path)
}

#: fn(book: data.frame, path: character) -> character
write_pdf_@ID@ <- function(book, path) {
  grDevices::pdf(path)
  on.exit(grDevices::dev.off())
  graphics::plot.new()
  #: NULL
  graphics::text(0.5, 0.5, paste("Positions:", nrow(book)))
  invisible(path)
}

#: fn(path: character) -> character[]
pdf_text_@ID@ <- function(path) {
  pdftools::pdf_text(path)
}
#@ test
test_that("reports @ID@ are written and readable", {
  withr::with_tempdir({
    book <- position_book_@ID@()
    write_workbook_@ID@(book, "book.xlsx")
    expect_equal(nrow(readxl::read_excel("book.xlsx")), @ROWS@)
    write_pdf_@ID@(book, "book.pdf")
    expect_match(pdf_text_@ID@("book.pdf"), "Positions: @ROWS@")
  })
})
