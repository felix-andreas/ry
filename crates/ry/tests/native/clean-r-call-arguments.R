# An argument R code wrote as a call is forced at the native entry when the body reads it before
# it runs anything else, as R would force it there. `swapped` reads its second parameter first,
# so the second argument's code runs first.
say <- function(text: Str, value: F64) -> F64 {
  cat(text, "\n")
  value
}
sq <- function(x: F64) -> F64 x * x
half <- function(x: F64) -> F64 x / 2
step <- function(x: F64) -> F64 half(sq(x)) + Num.sqrt(x)
swapped <- function(a: F64, b: F64) -> F64 b - a
total <- 0.0
for (i in 1:5) total <- total + step(Num.from_r(as.double(i)))
print(total)
print(swapped(say("a", 1), say("b", 2)))
print(step(Num.from_r(tryCatch(stop("no value"), error = function(e) 4))))
