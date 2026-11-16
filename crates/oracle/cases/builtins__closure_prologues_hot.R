# ifelse(), isTRUE(), isFALSE(), unname(), `%in%` and unlist() called often enough that the
# loop around them reaches every tier, and on the value shapes their native bodies must decline.
show <- function(value) { print(value); invisible(value) }
attempt <- function(expr) tryCatch(show(expr), error = function(e) cat("error:", conditionMessage(e), "\n"))

hot <- function(n) {
  values <- c(a = 1.5, b = NA, c = -2)
  parts <- list(p = 1:2, q = list(r = 3))
  total <- 0
  for (i in seq_len(n)) {
    x <- values[[i %% 3L + 1L]]
    total <- total + ifelse(is.na(x), 0, x * 2)
    if (isTRUE(!is.na(x))) total <- total + 1
    if (isFALSE(is.na(x) && i > 10L)) total <- total + 0.5
    total <- total + length(unname(values[i %% 3L + 1L]))
    if ((i %% 5L) %in% c(1L, 3L)) total <- total + 2
    total <- total + sum(unlist(parts)) + length(unlist(parts, use.names = FALSE))
  }
  total
}
show(hot(10L))
show(hot(3000L))
show(hot(3000L))

# Flags and tests that are compact sequences or carry attributes.
show(ifelse(1:1, "yes", "no"))
show(ifelse(array(TRUE, 1L), "yes", "no"))
show(unlist(list(a = 1, b = list(c = 2)), recursive = (1:1) == 1L))
show(unlist(list(a = 1), use.names = structure(TRUE, class = "flag")))
show(unlist(list(a = 1, b = list(c = 2)), recursive = structure(FALSE, class = "flag")))

# A global function with a base name does not change what the base closures do.
local({
  is.na <- function(x) stop("masked is.na")
  length <- function(x) stop("masked length")
  attributes <- function(x) stop("masked attributes")
  match <- function(...) stop("masked match")
  names <- function(x) stop("masked names")
  show(ifelse(TRUE, "yes", "no"))
  show(ifelse(NA, "yes", "no"))
  show(2 %in% c(1, 2))
  show(unname(c(a = 1)))
  show(unlist(list(a = 1, b = 2)))
})

# Branch values and objects the native bodies leave to the R bodies.
setClass("Box", representation(value = "numeric"))
box <- new("Box", value = 1)
attempt(isVirtualClass("Box"))
show(isS4(ifelse(TRUE, list(box), 1)[[1]]))
attempt(ifelse(TRUE, box, 1))
show(isS4(unname(box)))
noted <- structure(c(a = 1, b = 2), comment = "kept", tsp = c(1, 2, 1))
show(attributes(unname(noted)))
stamp <- as.POSIXct("2024-03-01 12:00:00", tz = "UTC")
show(stamp %in% (stamp + c(-1, 0, 1)))
show(as.Date("2024-03-01") %in% as.Date(c("2024-03-01", "2024-03-02")))
show((1:10) %in% 3:5)

# The function a forced argument sees is the base closure itself.
seen <- function() identical(sys.function(sys.parent()), unlist)
show(unlist(list(seen())))
seen_in <- function() identical(sys.function(sys.parent()), `%in%`)
show(seen_in() %in% TRUE)
