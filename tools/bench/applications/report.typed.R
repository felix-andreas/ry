# String building: format one report line per account, then join and measure the report.
report <- function(n: I64) -> F64 {
  padded <- function(digits: Str, width: I64) -> Str
    Str.concat(Str.repeat("0", width - Str.count_utf8_bytes(digits)), digits)
  lines <- List.map(List.range(1, n), function(i: I64) -> Str {
    cents <- i * 125
    amount <- f"{cents %/% 100}.{padded(Num.to_str(cents %% 100), 2)}"
    side <- if (i %% 2 == 0) "long" else "short"
    f"ACC{padded(Num.to_str(i), 6)};{amount};{side}"
  })
  text <- Str.join_with(lines, "\n")
  long <- List.count_if(lines, function(line) Str.count_utf8_bytes(Str.with_ascii_uppercased(line)) > 25)
  early <- List.count_if(lines, function(line) Str.starts_with(line, "ACC00"))
  Num.to_f64(Str.count_utf8_bytes(text) + long + early)
}
measure("report", function() report(20000))
