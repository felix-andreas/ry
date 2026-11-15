# The typed twin of sales.R, written in ry's own types: the table is one list per column, and the
# totals per region are a `Dict`. The regions are weighted in the order sales.R's `tapply` gives
# them, which is alphabetical.
pipeline <- function(region: List<Str>, units: List<I64>, price: List<F64>) -> F64 {
  totals <- Dict.empty()
  for (k in List.range(1, List.len(units))) {
    if (units[[k]] > 3) {
      revenue <- Num.to_f64(units[[k]]) * price[[k]]
      totals <- Dict.update(totals, region[[k]], function(found: Result<F64, :Missing>) -> Result<F64, :Missing> :Ok(Result.with_default(found, 0) + revenue))
    }
  }
  total <- function(name: Str) -> F64 Result.with_default(Dict.get(totals, name), 0)
  total("east") + 2 * total("north") + 3 * total("south") + 4 * total("west")
}
i <- seq_len(1000000L)
region <- List.from_r(c("north", "south", "east", "west")[i %% 4L + 1L])
units <- List.from_r(i %% 17L)
price <- List.from_r(1 + i %% 100L)
measure("sales", function() pipeline(region, units, price))
