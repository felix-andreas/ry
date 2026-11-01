---
title: Formatting rules
description: Every rule the formatter applies, with a before-and-after for each
---
<!-- R CODE IN THIS FILE IS FORMATTED AND SAVED TO docs/src/content/docs/reference/formatting-rules.md -->

`ry fmt` changes as little as it can: it normalizes spacing, indentation, and bracing, and keeps the
structure you wrote. [CLI](/reference/cli#fmt) lists its flags and exit codes.

## Philosophy

The formatter keeps your line breaks and never splits an expression you wrote on one line. Everything else follows from that idea:

* **Single-line expressions remain single-line:** The formatter adds line breaks only where the expression is already multi-line, and never breaks a single-line expression into several (with [one exception](#loops)).
* **Both nesting styles are preserved:** Compact ("hugged") and expanded forms for nested expressions are equally valid, and neither is rewritten into the other (see [hugging behavior](#hugging-behavior)).
* **Braces are added where omitting them invites a bug:** a multi-line `if` or function body gets braces so that a line added later cannot fall outside it (see [auto-bracing](#auto-bracing)), and every loop body is braced (see [loops](#loops)).

## Formatting Rules

### Binary operators

**Assignment operators** always have spaces around them:

```r
# assignment_operators : compare
x<-1
data<<-compute()
```

**Binary operators** have spaces around them, except for the range (`:`) and power (`^`) operators:

```r
# binary_operators : compare
result=x+y*z
power=base^exponent
sequence=1:10
```

**Pipeline operators:** continuation lines of a multi-line pipeline are indented one step:

```r
# pipeline_operators : compare
data %>%
filter(condition) %>%
select(value)
```

### Unary operators

`!`, unary `-`, and unary `+` hug their operand:

```r
# unary_operators : compare
result = ! condition
value = - 42
formula = ~x + y
```

`~` hugs a lone name (`~x`, `~.`) and is spaced before anything else (`~ x + y`, `~ log(x)`, `~ 1`).

### Blocks

**Multiline blocks** always have a newline after the opening brace and before the closing brace:

```r
# braced_blocks_multiline : compare
{ x <- 1
  print(x)
}
```

**Single-line blocks** are allowed, including those with semicolons. The formatter adds a space after `{` and before `}`:

```r
# braced_blocks_single_line : compare
{x <- 1; print(x)}
```

**Semicolons in multiline blocks** are split into separate lines, so that each statement gets its own line in a diff and its own breakpoint:

```r
# braced_blocks_semicolons_multiline : compare
{
  x <- 1; print(x)
}
```

**Empty blocks** have no space between the braces:

```r
# braced_expression_empty : compare
{  }
```

### Parenthesized expressions

A single-line parenthesized expression is always hugged, with no space between the parentheses and the expression inside:

```r
# parenthesized_expressions : compare
( x + y )
```

Multiline parenthesized expressions can be formatted in either hugged or expanded style; the formatter preserves both.

```r
# parenthesized_expressions_multiline : format
# expanded
(
  expression +
    other_part
)

# hugged
(expression +
    other_part)
```

### If expressions

**Single-line if-else:** Single-line `if-else` expressions are allowed and preserved, since `if` is an expression in R and can be used as a ternary operator:

```r
# conditional_statements_single_line : format
x <-if (condition) consequence else alternative
```

**Nested if-else:** A nested `if-else` chain is formatted so that each `else if` and each `else` starts on its own line, with every branch at the same indentation level and no extra indentation for nested cases.

```r
# conditional_statements_nested : format
if (a) {
  x
} else if (b) {
  y
} else {
  z
}
```

**Auto-bracing for multiline if-else:** Whenever an `if-else` spans multiple lines, all branches are wrapped in braces, so a statement added to one branch cannot end up outside it:

```r
# conditional_statements_multiline : compare
if (condition) {
  consequence
} else alternative
```

**Auto-bracing for multiline conditions:** If an `if` expression has a multiline condition, the formatter ensures the body is wrapped in braces even if it's a single expression:

```r
# if_statement_multiline_condition : compare
if (
  a && b
) body
```

### Loops

Loops are the only expressions that are **not allowed** on a single line.

`for`, `while`, and `repeat` loops are evaluated for their side effects and return no meaningful value, so the formatter writes them across multiple lines with explicit braces (see [auto-bracing](#auto-bracing)). This keeps side-effecting code visually distinct from expressions that produce a value.

**For loops** always get a braced body:

```r
# for_loops : compare
for(item in sequence) run_effect(item)
```

**While loops** follow similar block enforcement rules:

```r
# while_loops : compare
while(condition) action()
```

**Repeat loops** also enforce braced blocks:

```r
# repeat_loops : compare
repeat action()
```

**Multiline for loop headers:** Both of the following styles for multiline `for` loop headers are preserved:

```r
# for_loops_multline_head: format
for (
    item in sequence
) {}

for (
    item
    in sequence
) {}
```

### Function calls

A comma is followed by a space, and a named argument's `=` has a space on each side.

```r
# function_calls : compare
call(a,b=1,...)
```
**Multiline function calls:** Once two arguments appear on different lines, the call is treated as multiline, and each argument goes on its own line, so adding or removing one changes one line of the diff.

```r
# function_calls_multiline : compare
call(
  a = x,
  b = y, c = z
)
```

**Nested function calls** keep whichever of the hugged and expanded styles you wrote (see [hugging behavior](#hugging-behavior)).

**Compact multiline calls:** When a call spans multiple lines only because a single argument (often the last) is itself multiline, the other arguments stay on the original line:

```r
# mixed_line_format : format
# This format is preserved - only last argument spans multiple lines
call(a = x, b = y, c = inner(
  expr
))
```

Two common instances of this layout are a `test_that()` block and an S4 method definition:

```r
# test_that_and_s4_example: format
test_that("description", {
  expect_equal(result, expected)
})

setMethod("method", "Class", function(x) {
  # ... implementation
}, sealed = TRUE)
```

In the `setMethod` example, `sealed = TRUE` sits on a different line from the other arguments, but only the function body is multiline, so the formatter keeps the layout.

### Function definitions

**Single-line functions:** Functions with a simple, single-expression body can be written on one line, with or without braces.

```r
# function_definitions_single_line : format
add <- function(x, y) x + y
double <- function(x) { x * 2 }
```

**Multiline functions:** If the body spans several lines, braces are always added, even when the body starts on the same line as the function declaration.

```r
# function_definitions_multiline : compare
function()
    call(a = x, b = y)
```

**Exception – multiline call on same line:** If the function body is a function call that starts on the same line and is itself multiline, braces are not required.

```r
# function_definitions_multiline_call : format
fn <- function() call(
    a = x,
    b = y
  )
```

**Anonymous functions (lambda expressions):** Anonymous functions using `\` are formatted the same way as named functions, supporting both single-line and multiline bodies.

```r
# anonymous_functions_lambda : format
lapply(data, \(x) x + 1)

lapply(data, \(x) {
  y <- x * 2
  y + 1
})
```

### Switch calls

`switch()` calls are formatted like any other function call. A fallthrough case keeps a spaced `=` with nothing after it: `"b" = ,`.

```r
# switch_statements : format
result <- switch(
  type,
  "a" = handle_a(),
  "b" = ,
  "c" = handle_bc(),
  "default" = handle_default()
)
```

### Subsetting

**Bracket subsetting** follows the same formatting rules as function calls:

```r
# subsetting : compare
data[ row,col ]
data[[ "name" ]]
```

### Extract and namespace operators

**Extract and namespace operators** (`$`, `@`, `::`, `:::`) are formatted without spaces around them:

```r
# extract_operator : compare
collection $ item
collection @ item
pkg :: process
pkg ::: filter
```

When an extract or namespace chain spans several lines, each subsequent line is indented one step:

```r
# extract_operator_chained : compare
object$
call(x)$
call(x, y)
```

### String literals

String literals are normalized to double quotes (`"`), unless the string contains unescaped double quotes:

```r
# string_literals : compare
message <- 'Hello world'
quoted_content <- 'Say "hello"'
```

The inside of a multi-line string is never re-indented, because its whitespace is part of the value.

```r
# multiline_strings : compare
# { <- parent block gets deleted
    x <- "This is a multi-line string.
          It preserves
          indentation and line breaks."
# }
```

[Raw string literals](https://search.r-project.org/R/refmans/base/html/Quotes.html) (`r"(...)"`, `R"[...]"`, and their custom-dash variants) are preserved byte-for-byte. Quote normalization never applies to them, since their whole purpose is to hold characters, such as quotes and backslashes, that would otherwise need escaping.

```r
# raw_strings : compare
path <- r"(C:\Users\me)"
quoted <- r"(He said "hi")"
```

### Comments

The formatter ensures that a space is inserted after the `#` for standard comments. For special comment types like Roxygen (`#'`) and plumber (`#*`) comments, the space is placed after the initial two characters:

```r
# comments : compare
# comment with space
#comment without space
#'roxygen comment
#*plumber comment
#'string' <- commented out string
#!/usr/bin/env Rscript
```

Additional exceptions to this rule are:

- Commented-out strings such as `#'string'` are left unchanged, since inserting a space (e.g., `#' string'`) would alter the content of the string.
- [Shebangs](https://en.wikipedia.org/wiki/Shebang_(Unix)), for example `#!/usr/bin/env Rscript`, remain unchanged.
- [Quarto](https://quarto.org/docs/computations/execution-options.html) and knitr cell options, for example `#| echo: false`, are kept verbatim, since their `key: value` payload is read by machines.

### Type annotations

ry's type annotations are written in `#:` comments. The formatter treats each block of consecutive `#:` lines as one unit: the block is parsed with the same annotation grammar the type checker uses, and, only when it parses, re-rendered with the canonical spacing used throughout the [typing reference](/reference/type-system): one space after `#:`, no space before `,` or `:` and one space after, spaces around `|` and `->`, and no padding inside `(`, `[`, `{`, or generic `<...>`. A leading type-parameter binder such as `<T>` is followed by a space.

```r
# type_annotations_compact : compare
#:fn( x:integer,y:double )->character
render <- function(x, y) as.character(x + y)
#: list[ named : double ]
weights <- list(a = 1.0)
#: Either< E , A >|NULL
outcome <- NULL
```

Anything that does not parse as an annotation is left verbatim, beyond ensuring the single space after `#:`. Prose, a dotted type name, a `pkg::fun` reference, and a malformed type are all left alone, so the formatter never corrupts a comment it does not understand. Consecutive `#:` lines form one annotation block (exactly as the type checker groups them), so a block that is not a single valid annotation is also left as written. Two compact annotations with no blank line between them are one such block.

The reformat is deliberately non-invasive: token order, identifier casing, and your line breaks are preserved. A single-line annotation stays on one line, an expanded annotation (one written with `@param` / `@return` lines) keeps one directive per line rather than being collapsed into a compact `fn(...)`, and content lines are never rejoined.

```r
# type_annotations_expanded : compare
#: @param count { integer }
#: @param render { fn(integer)->character }
#: @returns { character }
```

When an annotation wraps across several `#:` lines, indentation and closing brackets are normalized from the shape of the opening brackets. An opening bracket followed by more content on its own line is *hugged*: its closer stays glued to the token before it. An opening bracket that ends its line is *expanded*: its closer gets a line of its own, aligned with the line that opened it. Each line is indented one step (`indent-width` spaces) per enclosing expanded bracket. Both canonical styles are stable:

```r
# type_annotations_two_styles : format
# expanded
#: @type Future {
#:   list{
#:     instrument: Instrument,
#:     maturity: integer
#:   }
#: }

# hugged
#: @type Future {list{
#:   instrument: Instrument,
#:   maturity: integer
#: }}
```

and mixed closer shapes normalize to the nearest consistent style:

```r
# type_annotations_mixed : compare
#: @type Future {
#:   list{
#:     instrument: Instrument,
#:     maturity: integer
#: }}
```

A blank line, a non-`#:` comment, or ordinary code ends an annotation block, so unrelated comments are never pulled into one. Trailing empty `#:` lines at the end of a block are dropped.

### Inline type annotations

An [inline annotation](/reference/type-system#inline-annotation-syntax) is rendered on one line with the same spacing as a `#:` type: no space before `:` and one after, spaces around `|` and `->`, and no padding inside brackets. The `<...>` of a function's type parameters hugs `function` and the parameter list, and the return type sits between the parameter list and the body with one space on each side. A type written across several lines is joined onto one, unless it holds a comment, in which case it is kept as written.

```r
# inline_annotations : compare
count:integer<-0L
add<-function(x:integer,y:  list[ named:double ]=list())->double{x}
pick <- function <T:numeric> (x : T) -> T x
```

### Line spacing

The formatter normalizes line spacing between expressions, allowing at most one empty line:

```r
# line_spacing : compare
x <- 1
y <- 2


z <- 3
```

A single blank line inside a call is kept too, so an R6 class keeps the blank lines between its methods:

```r
# r6_class_definitions : format
PersonClass <- R6Class(
  "Person",
  public = list(
    initialize = function(name) {
      private$name <- name
    },

    get_name = function() {
      return(private$name)
    }
  )
)
```

### Line endings and indentation

The line ending follows `[format] line-ending`, whose default `"auto"` keeps the file's own (`LF` or `CRLF`), read from the first line break after the first code or comment. The indent is `[format] indent-width` spaces, 2 by default.

### Formatting a selection

Formatting a selection (`ry fmt --range`, or an editor's "Format Selection") lays out the lines the selection touches exactly as formatting the whole file lays them out, and leaves every other line byte for byte as it was. The selection widens to whole lines, and statements are the unit: selecting one line of a long function rewrites only that line's statement, but selecting one argument of a call rewrites the whole call, because how a call breaks depends on all of it. A comment on its own line is a unit of its own.

Selected lines keep the indentation step the file already uses, read from the statements the formatter places (the commonest step wins), so tidying part of a file indented by four spaces or tabs does not move those lines out of line with their neighbours. Formatting the whole file is what re-indents it to `indent-width`. A `# fmt: off` region or `# fmt: skip` exemption inside the selection is respected, and a file that does not parse refuses exactly as whole-file formatting does.

On-type formatting is the same operation on one line: a line break lays out the line it finished (never the new line under the cursor), and `}` lays out the construct it closes.

## Format Suppression

A `# fmt: skip` comment turns formatting off for one expression, for example to keep a hand-aligned data structure. The skipped expression is preserved byte-exactly, including the column of its first line, so hand-aligned constructs keep their alignment.

The `fmt: skip` directive can be placed before any expression to skip formatting for it:

```r
# format_suppression : format
matrix(
  # fmt: skip
  c(
    1, 2,
    3, 4
  ), # only the c(..) call won't be reformatted
  nrow = 2
) 
```

Or, at the end of a line to skip the previous expression:

<!-- The fence below says R, not r, on purpose: the doc harness formats every ```r block, which would
     defeat a block whose whole point is that `# fmt: skip` left it alone. The cost is that this one
     block renders without syntax highlighting. Do not "fix" the capital R. -->

```R
# the entire matrix(..) call won't be reformatted
matrix(c(1, 2,
         3, 4), nrow=2) # fmt: skip
```

For a whole region, use `# fmt: off` and re-enable formatting with `# fmt: on`. The region between the directives is preserved byte-exactly, keeping its original indentation, columns, and blank lines. A directive must start its own line: a comment trailing code, such as `y <- 1 # fmt: off`, is an ordinary comment. The directives work at the top level as well as inside functions and `{ ... }` blocks; a region left open runs to the end of its enclosing block.

```r
# format_suppression_off_on : format
f <- function() {
  # fmt: off
    x  <-  c(1,   2,
             30,  4)
  # fmt: on
  x
}
```

`# fmt: skip-file` before any other code or comment (a shebang included) leaves the whole file untouched. Blank lines before it are allowed.

## Rationale

### Auto-bracing

**Accidental bugs:** Omitting braces in loops, function definitions, or `if` expressions makes a later edit change what the code means. Adding a line after an unbraced `if` leaves only the first line under the condition:

```r
# _ : skip
# unbraced condition
if (condition)
  line1
  line2 # <- is meant to be in body

# how it is interpreted:
if (condition)
  line1
line2 # <- gets executed unconditionally
```

Therefore, the formatter always adds braces to **`if` expressions** and **function definitions** whenever the body spans multiple lines.

```r
# auto_bracing_conditional_statements : compare
if (condition)
  action()
```

For a control flow structure such as a `for`, `while`, or `repeat` loop, the formatter always adds braces around the body, whatever its length, because a single-line loop is not allowed. See [Loops](#loops).

```r
# auto_bracing_for : compare
for (item in sequence)
  action()
```

### Hugging behavior

"Hugging" is the compact layout for a nested expression in a multiline context: the inner expression starts on the same line as the outer expression's opening delimiter. Both hugged and expanded forms are allowed, and the formatter preserves whichever was written.

**Nested function calls** can be formatted in a hugged style:

```r
# hugging_nested_function_calls : format
# Hugged format - both functions start on the same line
result <- outer(inner(
  arg
))

# Expanded format - also valid
result <- outer(
  inner(
    arg
  )
)
```

**Parenthesized expressions** can also use hugging:

```r
# hugging_parenthesized : format
(expression +
  other_part)

# Also allowed
(
  expression +
    other_part
)
```

See [compact multiline calls](#function-calls) under Function calls.
