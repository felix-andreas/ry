# blocked: renamed
alias User = Record{name: Str, age: I64}
renamed <- function(user: User, name: Str) -> Str {
  user$name <- name
  user$name
}
main <- function(name: Str) -> Str renamed((name = "ada", age = 36), name)
cat(main("grace"), "\n")
