# List processing with failures: parse "name,age,score,team" rows, reject the invalid ones, and
# total the valid scores per team.
alias Person = Record{name: Str, age: I64, score: F64, team: Str}

parse_row <- function(row: Str) -> Result<Person, :BadAge | :BadScore | :Minor> {
  parts <- Str.split_on(row, ",")
  match (Str.to_i64(parts[[2]])) {
    :Err(_) => :Err(:BadAge)
    :Ok(age) => if (age < 18) :Err(:Minor) else match (Str.to_f64(parts[[3]])) {
      :Err(_) => :Err(:BadScore)
      :Ok(score) => :Ok((name = parts[[1]], age = age, score = score, team = parts[[4]]))
    }
  }
}
team_scores <- function(rows: List<Str>) -> F64 {
  parsed <- List.map(rows, parse_row)
  people <- List.keep_oks(parsed, function(p: Result<Person, :BadAge | :BadScore | :Minor>) -> Result<Person, :BadAge | :BadScore | :Minor> p)
  totals <- List.walk(people, Dict.empty(), function(seen: Dict<Str, F64>, person: Person) -> Dict<Str, F64> {
    Dict.update(seen, person$team, function(found: Result<F64, :Missing>) -> Result<F64, :Missing> :Ok(Result.with_default(found, 0) + person$score))
  })
  errors <- Num.to_f64(List.len(parsed) - List.len(people))
  score <- function(team: Str) -> F64 Result.with_default(Dict.get(totals, team), 0)
  score("red") + 2 * score("blue") + 3 * score("green") + errors
}
i <- seq_len(60000L)
rows <- List.from_r(paste0("user", i, ",", ifelse(i %% 17L == 0L, "x", as.character(i %% 90L)), ",", (i %% 50L) / 2, ",", c("red", "blue", "green")[i %% 3L + 1L]))
measure("validate", function() team_scores(rows))
