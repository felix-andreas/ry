# List processing with failures: parse "name,age,score,team" rows, reject the invalid ones, and
# total the valid scores per team.
alias Person = Record{name: character, age: integer, score: double, team: character}

parse_row <- function(row: character) -> Result<Person, :BadAge | :BadScore | :Minor> {
  parts <- Str.split_on(row, ",")
  match (Str.to_integer(parts[[2L]])) {
    :Err(_) => :Err(:BadAge)
    :Ok(age) => if (age < 18L) :Err(:Minor) else match (Str.to_double(parts[[3L]])) {
      :Err(_) => :Err(:BadScore)
      :Ok(score) => :Ok((name = parts[[1L]], age = age, score = score, team = parts[[4L]]))
    }
  }
}
team_scores <- function(rows: list[character]) -> double {
  parsed <- List.map(rows, parse_row)
  people <- List.keep_oks(parsed, function(p: Result<Person, :BadAge | :BadScore | :Minor>) -> Result<Person, :BadAge | :BadScore | :Minor> p)
  totals <- List.walk(people, Dict.empty(), function(seen: Dict<character, double>, person: Person) -> Dict<character, double> {
    Dict.update(seen, person$team, function(found: Result<double, :Missing>) -> Result<double, :Missing> :Ok(Result.with_default(found, 0) + person$score))
  })
  errors <- List.len(parsed) - List.len(people)
  score <- function(team: character) -> double Result.with_default(Dict.get(totals, team), 0)
  score("red") + 2 * score("blue") + 3 * score("green") + errors
}
i <- seq_len(60000L)
rows: list[character] <- List.from_vector(paste0("user", i, ",", ifelse(i %% 17L == 0L, "x", as.character(i %% 90L)), ",", (i %% 50L) / 2, ",", c("red", "blue", "green")[i %% 3L + 1L]))
measure("validate", function() team_scores(rows))
