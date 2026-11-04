# Validation and configuration code: small functions with defaults, `...` forwarding, match.arg,
# stopifnot, missing(), match.call() for logging, and option lookups. Production packages run this
# kind of code at every API boundary.
`%||%` <- function(a, b) if (is.null(a)) b else a

check_scalar <- function(x, type = c("numeric", "character", "logical"), name = deparse(substitute(x)),
                         allow_na = FALSE) {
  type <- match.arg(type)
  ok <- switch(type,
    numeric = is.numeric(x),
    character = is.character(x),
    logical = is.logical(x)
  )
  if (!ok || length(x) != 1L) stop(sprintf("`%s` must be a single %s value", name, type), call. = FALSE)
  if (!allow_na && is.na(x)) stop(sprintf("`%s` must not be NA", name), call. = FALSE)
  invisible(x)
}

check_range <- function(x, lower = -Inf, upper = Inf, ...) {
  check_scalar(x, "numeric", ...)
  stopifnot(is.finite(lower) || lower == -Inf, x >= lower, x <= upper)
  x
}

new_settings <- function(currency = c("EUR", "USD", "GBP", "CHF"), horizon = 12, confidence = 0.95,
                         method = c("historical", "parametric", "montecarlo"), weights = NULL, ...) {
  currency <- match.arg(currency)
  method <- match.arg(method)
  check_range(horizon, 1, 120, name = "horizon")
  check_range(confidence, 0.5, 0.999, name = "confidence")
  extra <- list(...)
  if (length(extra) && is.null(names(extra))) stop("extra settings must be named")
  settings <- list(
    currency = currency, horizon = as.integer(horizon), confidence = confidence,
    method = method, weights = weights %||% c(equity = 0.6, bonds = 0.4)
  )
  structure(utils::modifyList(settings, extra), class = "settings")
}

describe_call <- function(...) {
  call <- match.call()
  paste(names(call)[-1], collapse = ",")
}

settings_key <- function(settings, digits = 3) {
  fields <- vapply(names(settings), function(field) {
    value <- settings[[field]]
    if (is.numeric(value)) paste(format(round(value, digits), nsmall = digits), collapse = "/")
    else paste(value, collapse = "/")
  }, character(1))
  paste(names(fields), fields, sep = "=", collapse = ";")
}

validate_row <- function(row, required = c("id", "amount", "currency", "date"), strict = TRUE) {
  missing_fields <- setdiff(required, names(row))
  if (length(missing_fields)) {
    if (strict) stop("missing fields: ", paste(missing_fields, collapse = ", "))
    return(FALSE)
  }
  is.numeric(row$amount) && !is.na(row$amount) && row$currency %in% c("EUR", "USD", "GBP", "CHF") &&
    nchar(row$date) == 10L
}

option_or <- function(name, default = NULL) {
  value <- getOption(paste0("bench.", name))
  if (is.null(value)) default else value
}

validation <- function(n) {
  currencies <- c("EUR", "USD", "GBP", "CHF")
  methods <- c("historical", "parametric", "montecarlo")
  keys <- character(n)
  valid <- 0L
  calls <- character(0)
  for (i in seq_len(n)) {
    s <- new_settings(
      currency = currencies[[i %% 4L + 1L]], horizon = i %% 60 + 1, confidence = 0.9 + (i %% 9) / 100,
      method = methods[[i %% 3L + 1L]], label = paste0("run", i), rounding = option_or("rounding", 2L)
    )
    keys[[i]] <- settings_key(s)
    row <- list(id = i, amount = i * 1.5, currency = s$currency, date = sprintf("2024-%02d-%02d", i %% 12 + 1, i %% 28 + 1))
    if (i %% 7L == 0L) row$amount <- NA_real_
    if (validate_row(row, strict = FALSE)) valid <- valid + 1L
    if (i %% 50L == 0L) calls <- c(calls, describe_call(a = i, b = s$method, c = row$date))
    bad <- tryCatch(check_scalar(c(1, 2), "numeric", name = "pair"), error = function(e) conditionMessage(e))
  }
  list(valid = valid, keys = length(unique(keys)), first = keys[[1]], calls = length(calls), bad = bad)
}

measure("validation", function() validation(3000L))
