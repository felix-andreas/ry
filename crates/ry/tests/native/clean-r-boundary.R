# R functions called where a native function may call them: as a statement, or inside a conversion.
# The first call runs in the interpreter, which loads `stats::rnorm`, so the second one is the
# call that has to run natively.
simulate <- function(draws: I64, label: Str) -> F64 {
  set.seed(Num.to_r(draws))
  noise <- List.from_r(stats::rnorm(Num.to_r(draws)))
  low_bits <- Num.from_r(bitwAnd(Num.to_r(draws), 3L))
  doubled <- Str.from_r(strrep(label, 2L))
  exists <- Bool.from_r(file.exists(label))
  if (exists) stop(sprintf("a file is named %s", toupper(label)))
  cat(doubled, "\n")
  message("drew ", List.to_r(noise))
  List.sum(noise) * 0 + Num.to_f64(low_bits)
}
cat(simulate(4, "native"), "\n")
cat(simulate(5, "native"), "\n")
