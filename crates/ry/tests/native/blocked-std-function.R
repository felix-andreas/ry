# blocked: checked
checked <- function(first: F64) -> Bool {
  positive <- List.for_each_try([first, 2, 3], function(x: F64) -> Result<NULL, :Negative> {
    if (x < 0) :Err(:Negative) else :Ok(NULL)
  })
  Result.is_ok(positive)
}
print(checked(1))
