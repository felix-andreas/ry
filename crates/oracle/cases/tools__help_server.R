# The dynamic help server answers requests from the session itself: GNU R's download loop runs
# the event loop, so a session can read its own help pages.
options(help.ports = 31871:31890)
invisible(tools::startDynamicHelp(TRUE))
port <- tools:::httpdPort()
stopifnot(port > 0L, identical(tools::startDynamicHelp(NA), port))
fetch <- function(path) {
  con <- url(sprintf("http://127.0.0.1:%d%s", port, path))
  on.exit(close(con))
  readLines(con, warn = FALSE)
}
# A failed request's message names the URL, whose port changes from run to run.
status <- function(path) {
  tryCatch(fetch(path), error = function(e) gsub(port, "PORT", conditionMessage(e)),
    warning = function(w) gsub(port, "PORT", conditionMessage(w)))
}

page <- fetch("/library/stats/html/lm.html")
length(page)
grep("<title>", page, value = TRUE)
head(fetch("/doc/html/R.css"), 2)
head(fetch("/library/base/html/00Index.html"), 3)

handlers <- tools:::.httpd.handlers.env
handlers[["probe"]] <- function(path, query, body, headers) {
  if (grepl("echo", path)) {
    return(list(paste(c(path, names(query), query), collapse = "|"), "text/plain"))
  }
  if (grepl("raw", path)) return(list(charToRaw("bytes"), "application/octet-stream", "X-Probe: 1", 201L))
  if (grepl("fail", path)) stop("handler failed")
  if (grepl("string", path)) return("a plain string")
  if (grepl("invalid", path)) return(list())
  list(payload = rawToChar(headers), "text/plain")
}
fetch("/custom/probe/echo/a%20b?x=1&y=a+b%26c&&z")
fetch("/custom/probe/echo?")
fetch("/custom/probe/raw")
status("/custom/probe/fail")
status("/custom/probe/string")
status("/custom/probe/invalid")
grep("^Request-Method", fetch("/custom/probe/headers"), value = TRUE)
rm("probe", envir = handlers)

tools::startDynamicHelp(FALSE)
tools:::httpdPort()
