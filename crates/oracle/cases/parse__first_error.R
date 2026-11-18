# parse() stops at the first error its parser meets. The lexer fails on the token it is reading, a
# check the grammar makes on a finished construct (a parameter declared twice, the right side of a
# pipe, an empty name) fails when the token that closes the construct is read, and a placeholder
# outside a pipe fails at the end of its statement. A text with several errors reports the first.
report <- function(texts) {
  for (text in texts) {
    result <- tryCatch(
      {
        parse(text = text, keep.source = FALSE)
        "ok"
      },
      error = function(e) paste(class(e)[1], "|", conditionMessage(e))
    )
    cat(sprintf("%-32s => %s\n", encodeString(text), gsub("\n", "\\\\n", result)))
  }
}

# One error each: the class, the message and the position.
report(c(
  "function(a, a) 1", "\\(a, a) 1", "function(a, `a`) 1", "function(abc,\n  abc) 1",
  "function(..., ...) 1", "function(a, b = function(a, a) 1) 1", "function(a, a, a) 1",
  "x |> y", "x |> 1", "x |> -1", "x |> `+`(1)", "x |> 'if'(1)", "x |> return()", "x |> (f)()",
  "x |> f(_)", "x |> f(y = _, z = _)", "x |> f(_, y = _)", "x |> _$a(b = _)", "x |> _$a[_]",
  "x |> f(g(_))", "x |> f(y = (_))", "x |> _", "x |> (_)", "_", "f(\n_)", "1; _\n", "{\n_\n}\n",
  "function() _", "1 -> _", "x$_", "_$a", "if (_) 1", "x |> f(y = _) + _",
  "'a\\x'", "'\\x41\\u00e4'", "'\\400'", "'\\0'", "'\\U{110000}'", "`a\\q`", "`\\x`", "`a\\0b`",
  "1;;2", ";", "1\n;", "f(a,, b)\n;;", "x |> y => f(y)",
  "``", "x$``", "x@``", "function(``) 1", "`` <- 1", "``::a", "for (`` in x) 1",
  "\"\"()", "''(1)", "r\"()\"()", "x |> \"\"()", "c(\"\" = 1)", "x[[\"\" = 1]]", "c(a = , \"\" = )"
))

# What R accepts.
report(c(
  "x |> f(y = _)", "x |> _$a[[1]]", "x |> f(y = _)$a", "x |> f(a) |> _[1]", "function(a = _) 1",
  "x |> `_`()", "f(`_`)", "x |> f(`_` = 1)", "x |> f()()", "x |> a::b()", "{;}", "1;\n2",
  "x$\"\"", "x@\"\"", "\"\" <- 1", "\"\"::a", "(\"\")()", "f(\"\")", "x |> \n f()"
))

# Two errors: the first one R meets.
report(c(
  "function(a, a) )", "function(a, a", "function(a, a = 1 +", "function(a, a '\\q'",
  "function(a, a) '\\q'", "function(a, a) function(b, b) 1", "function(a, a \"",
  "x |> f )", "x |> f '\\q'", "x |> g\n'\\q'", "x |> y )", "x |> y \"", "x |> y +", "x |> f(1 +",
  "x |> f(_) |> g(_)", "x |> f(_) \"", "c(1 2) |> y", "(x |> y\n'\\q')", "{x |> y\n'\\q'}",
  "'\\q' )", ") '\\q'", "'\\x41' 'a\\q'", "1 2 '\\q'", "'\\q' \"",
  "{ _ }\n)", "(_", "_ )", "_ +", "f(_) \"", "f(_)\n\"", "x |> f(y = _)\n_",
  "`` )", ") ``", "`` '\\q'", "'\\q' ``", "x |> y ``", "x |> y\n``", "function(a, a) ``",
  "\"\"() )", ") \"\"()", "\"\"(", "\"\"()(", "''() '\\q'", "''() \"", "f(''()) \"", "x |> ''() \"",
  "c(\"\" = 1) )", "c(\"\" = 1", "c('' = 1 \"", "{ 1; \"\"() \n )"
))

# A construct the grammar refuses a part of never reaches its check.
report(c(
  "r\"()\"(-)", "''(1 +)", "c('' = -)", "c('' = 1 +)", "c('' = if (x) )", "c('' = (1) 2)",
  "x |> y $ )", "x |> y + )", "x |> y(1 +)", "x |> f(_, -)", "function(a, a = ) 1",
  "function(a, a = -) 1", "function(a, a = b c) 1", "f('' = function(a, a) 1 2)",
  "f(function(a, a = ) 1, '' = 2)"
))
