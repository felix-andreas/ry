#@ weight 2
#@ param ITEMS int 2 10
# Parsing XML and HTML documents held in strings.
make_document_@ID@ <- function(n = @ITEMS@L) {
  items <- sprintf("<item id=\"%d\"><name>name %d</name><value>%d</value></item>", seq_len(n), seq_len(n), seq_len(n) * 3L)
  paste0("<root>", paste(items, collapse = ""), "</root>")
}

parse_items_@ID@ <- function(text) {
  doc <- xml2::read_xml(text)
  nodes <- xml2::xml_find_all(doc, "//item")
  data.frame(
    id = as.integer(xml2::xml_attr(nodes, "id")),
    name = xml2::xml_text(xml2::xml_find_first(nodes, "name")),
    value = as.numeric(xml2::xml_text(xml2::xml_find_first(nodes, "value")))
  )
}

scrape_links_@ID@ <- function(html) {
  page <- rvest::minimal_html(html)
  anchors <- rvest::html_elements(page, "a")
  data.frame(
    label = rvest::html_text2(anchors),
    href = rvest::html_attr(anchors, "href")
  )
}
#@ test
test_that("markup @ID@ parses", {
  items <- parse_items_@ID@(make_document_@ID@())
  expect_equal(nrow(items), @ITEMS@)
  expect_equal(items$value, seq_len(@ITEMS@) * 3)
  links <- scrape_links_@ID@("<p><a href='/one'>One</a> <a href='/two'>Two</a></p>")
  expect_equal(links$href, c("/one", "/two"))
  expect_equal(links$label, c("One", "Two"))
})
