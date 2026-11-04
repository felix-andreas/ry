# String handling: building messages with paste and sprintf, cleaning identifiers with gsub and
# regmatches, splitting, padding and formatting numbers, and the same through stringr. Logging,
# report generation and input parsing are made of this.
to_snake <- function(x) {
  x <- gsub("([a-z0-9])([A-Z])", "\\1_\\2", x)
  x <- gsub("[^A-Za-z0-9]+", "_", x)
  tolower(gsub("^_+|_+$", "", x))
}

format_amount <- function(x, currency = "EUR", digits = 2L) {
  paste(currency, formatC(x, format = "f", digits = digits, big.mark = ","))
}

parse_line <- function(line) {
  parts <- strsplit(line, "|", fixed = TRUE)[[1]]
  fields <- trimws(parts)
  values <- regmatches(fields[[3]], regexpr("[0-9]+\\.[0-9]+", fields[[3]]))
  list(id = as.integer(fields[[1]]), name = to_snake(fields[[2]]), value = as.numeric(values))
}

log_message <- function(level, fmt, ...) {
  sprintf("[%s] %-5s %s", "2024-01-01 00:00:00", toupper(level), sprintf(fmt, ...))
}

strings <- function(n) {
  names <- c("MarketValue", "accrued Interest", "Yield-To-Maturity", "durationModified", "Z Spread")
  lines <- sprintf("%d | %s %d | value=%0.3f;unit=bp", seq_len(n), names[seq_len(n) %% 5L + 1L],
                   seq_len(n), seq_len(n) / 7)
  parsed <- lapply(lines, parse_line)
  ids <- vapply(parsed, function(p) p$id, integer(1))
  keys <- vapply(parsed, function(p) p$name, character(1))
  amounts <- vapply(parsed, function(p) p$value, numeric(1))
  messages <- character(n)
  for (i in seq_len(n)) {
    messages[[i]] <- log_message(if (i %% 10L == 0L) "warn" else "info", "%s for %s: %s",
                                 keys[[i]], ids[[i]], format_amount(amounts[[i]] * 1000))
  }
  padded <- stringr::str_pad(keys, 24, side = "right", pad = ".")
  detected <- sum(stringr::str_detect(messages, "WARN"))
  replaced <- stringr::str_replace_all(messages, "[0-9]{3,}", "#")
  pieces <- stringr::str_split_fixed(keys, "_", 3)
  joined <- stringr::str_c(pieces[, 1], pieces[, 2], sep = "-")
  wide <- vapply(split(keys, ids %% 50L), function(k) paste(sort(unique(k)), collapse = ","), character(1))
  list(
    first = messages[[1]], last = replaced[[n]], detected = detected,
    widths = sum(nchar(padded)), joined = table(joined), wide = wide[[1]],
    upper = sum(nchar(toupper(messages))), counts = table(substr(keys, 1, 3))
  )
}

measure("strings", function() strings(3000L))
