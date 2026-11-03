# List processing with failures: parse "name,age,score,team" rows, reject the invalid ones, and
# total the valid scores per team.
parse_row <- function(row) {
  parts <- strsplit(row, ",", fixed = TRUE)[[1L]]
  age <- suppressWarnings(as.integer(parts[[2L]]))
  if (is.na(age)) return(list(ok = FALSE, error = "bad_age"))
  if (age < 18L) return(list(ok = FALSE, error = "minor"))
  list(ok = TRUE, value = list(name = parts[[1L]], age = age, score = as.double(parts[[3L]]), team = parts[[4L]]))
}
team_scores <- function(rows) {
  parsed <- lapply(rows, parse_row)
  totals <- list()
  errors <- 0L
  for (p in parsed) {
    if (p$ok) {
      team <- p$value$team
      old <- totals[[team]]
      totals[[team]] <- (if (is.null(old)) 0 else old) + p$value$score
    } else {
      errors <- errors + 1L
    }
  }
  totals[["red"]] + 2 * totals[["blue"]] + 3 * totals[["green"]] + errors
}
i <- seq_len(60000L)
rows <- paste0("user", i, ",", ifelse(i %% 17L == 0L, "x", as.character(i %% 90L)), ",", (i %% 50L) / 2, ",", c("red", "blue", "green")[i %% 3L + 1L])
measure("validate", function() team_scores(rows))
