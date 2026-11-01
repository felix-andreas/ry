# iconvlist() asks .Internal(iconv()) for the encoding names with a NULL x, and readr checks
# every file's encoding against that list.
listed <- .Internal(iconv(NULL, "", "", "", TRUE, FALSE))
print(is.null(listed))
encodings <- iconvlist()
print(is.character(encodings))
print(all(c("UTF-8", "LATIN1") %in% toupper(encodings)))
