---
title: Formatting rules
description: Every rule the formatter applies, with a before-and-after for each
---
<!-- THIS FILE IS GENERATED AUTOMATICALLY. MAKE CHANGES TO crates/format/tests/formatter.template.md INSTEAD -->

ry includes a non-invasive R code formatter. It normalizes spacing, indentation and bracing, and preserves the structure you wrote.

## Usage

Format your R files using the command line:

```sh
ry fmt           # Format all files in the current directory
ry fmt <path>    # Format all files in <path>
ry fmt --check   # List files that would be reformatted, without writing
ry fmt --diff    # Show a diff of formatting changes without applying them
```

`--check` and `--diff` exit 1 when any file would change, which is what a CI job gates on. Errors
(for example a file that cannot be parsed) exit 2. See the full
[exit-code table](/reference/cli#exit-codes).

## Philosophy

The formatter preserves existing line breaks and does not split expressions that are written on one line. Three principles follow from that:

* **Single-line expressions remain single-line:** The formatter adds line breaks only where the expression is already multi-line, and never breaks a single-line expression into several (with [one exception](#loops)).
* **Both nesting styles are preserved:** Compact ("hugged") and expanded forms for nested expressions are equally valid, and neither is rewritten into the other (see [hugging behavior](#hugging-behavior)).
* **Braces are added only where they prevent a bug:** Auto-bracing applies where omitting braces changes what a later edit means (see [auto-bracing](#auto-bracing)).

## Formatting Rules

The rules below describe the formatter's behavior for each kind of expression, including the edge cases that receive special handling.

### Binary operators

**Assignment operators** always have spaces around them:

```r
# Before formatting
x<-1
data<<-compute()

# After formatting
x <- 1
data <<- compute()
```

**Binary operators** have spaces around them, except for the range (`:`) and power (`^`) operators:

```r
# Before formatting
result=x+y*z
power=base^exponent
sequence=1:10

# After formatting
result = x + y * z
power = base^exponent
sequence = 1:10
```

**Pipeline operators** maintain proper indentation when expressions span multiple lines:

```r
# Before formatting
data %>%
filter(condition) %>%
select(value)

# After formatting
data %>%
  filter(condition) %>%
  select(value)
```

### Unary operators

Unary operators receive appropriate spacing based on their type and context:

```r
# Before formatting
result = ! condition
value = - 42
formula = ~x + y

# After formatting
result = !condition
value = -42
formula = ~ x + y
```

**Special spacing rule:** The `~` (formula) operator gets a space when followed by complex expressions, but not when followed by simple identifiers.

### Blocks

**Multiline blocks** always have a newline after the opening brace and before the closing brace:

```r
# Before formatting
{ x <- 1
  print(x)
}

# After formatting
{
  x <- 1
  print(x)
}
```

**Single-line blocks** are allowed, including those with semicolons. The formatter adds a space after `{` and before `}` for readability:

```r
# Before formatting
{x <- 1; print(x)}

# After formatting
{ x <- 1; print(x) }
```

**Semicolons in multiline blocks** are split into separate lines for clarity:

```r
# Before formatting
{
  x <- 1; print(x)
}

# After formatting
{
  x <- 1
  print(x)
}
```

**Empty blocks** have no space between the braces:

```r
# Before formatting
{  }

# After formatting
{}
```

### Parenthesized expressions

A single-line parenthesized expression is always formatted in a hugging style. There is no extra space between the opening parenthesis and the enclosed expression.

```r
# Before formatting
( x + y )

# After formatting
(x + y)
```

Multiline parenthesized expressions can be formatted in either hugged or expanded style; the formatter preserves both.

```r
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
x <- if (condition) consequence else alternative
```

**Nested if-else.** A nested `if-else` chain is formatted so that each `else if` and each `else` starts on its own line. All branches align at the same indentation level, and a nested case gets no extra indentation.

```r
if (a) {
  x
} else if (b) {
  y
} else {
  z
}
```

**Auto-bracing for multiline if-else:** Whenever an `if-else` spans multiple lines, all branches are always wrapped in braces for clarity and consistency:

```r
# Before formatting
if (condition) {
  consequence
} else alternative

# After formatting
if (condition) {
  consequence
} else {
  alternative
}
```

**Auto-bracing for multiline conditions:** If an `if` expression has a multiline condition, the formatter ensures the body is wrapped in braces even if it's a single expression:

```r
# Before formatting
if (
  a && b
) body

# After formatting
if (
  a && b
) {
  body
}
```

### Loops

Loops are the only expressions that are **not allowed** on a single line.

`for`, `while`, and `repeat` loops are evaluated for their side effects and return no meaningful value, so the formatter writes them across multiple lines with explicit braces (see [auto-bracing](#auto-bracing)). This keeps side-effecting code visually distinct from expressions that produce a value.

**For loops** always enforce braced blocks for the body, ensuring consistency:

```r
# Before formatting
for(item in sequence) run_effect(item)

# After formatting
for (item in sequence) {
  run_effect(item)
}
```

**While loops** follow similar block enforcement rules:

```r
# Before formatting
while(condition) action()

# After formatting
while (condition) {
  action()
}
```

**Repeat loops** also enforce braced blocks:

```r
# Before formatting
repeat action()

# After formatting
repeat {
  action()
}
```

**Multiline for loop headers:** Both of the following styles for multiline `for` loop headers are preserved:

```r
for (
  item in sequence
) {}

for (
  item
  in sequence
) {}
```

### Function calls

Function calls receive consistent formatting with proper spacing around argument separators and assignment operators.

```r
# Before formatting
call(a,b=1,...)

# After formatting
call(a, b = 1, ...)
```
**Multiline function calls:** Once two arguments appear on different lines, the call is treated as multiline, and each argument is formatted on its own line for clarity.

```r
# Before formatting
call(
  a = x,
  b = y, c = z
)

# After formatting
call(
  a = x,
  b = y,
  c = z
)
```

**A nested function call** uses either a hugged style or an expanded style. In the hugged style the inner call starts directly after the outer call's parenthesis. The formatter preserves both.

```r
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

**Compact multiline calls:** When a call spans multiple lines solely because a single argument (often the final one) is itself multiline, the formatter keeps the other arguments on the original line rather than placing every argument on its own line, preserving a concise, compact layout:

```r
# This format is preserved - only last argument spans multiple lines
call(a = x, b = y, c = inner(
  expr
))
```


```r
test_that("description", {
  expect_equal(result, expected)
})

setMethod("method", "Class", function(x) {
  # ... implementation
}, sealed = TRUE)
```

In the `setMethod` example: Even though `sealed = TRUE` is on a different line than the other arguments, only the function body is multiline, so the formatter preserves this layout.

**Note:** You can always opt in to the fully expanded multiline style: if you add a newline so that at least two arguments of a call are on different lines, the formatter treats it as multiline and will place every argument on its own line.

### Function definitions

**Single-line functions:** Functions with a simple, single-expression body can be written on one line, with or without braces.

```r
add <- function(x, y) x + y
double <- function(x) { x * 2 }
```

**Multiline functions.** If the function body spans several lines, braces are always added, even when the body starts on the same line as the function declaration.

```r
# Before formatting
function()
    call(a = x, b = y)

# After formatting
function() {
  call(a = x, b = y)
}
```

**Exception – multiline call on same line:** If the function body is a function call that starts on the same line and is itself multiline, braces are not required.

```r
fn <- function() call(
  a = x,
  b = y
)
```

**Anonymous functions (lambda expressions):** Anonymous functions using `\` are formatted the same way as named functions, supporting both single-line and multiline bodies.

```r
lapply(data, \(x) x + 1)

lapply(data, \(x) {
  y <- x * 2
  y + 1
})
```

### Switch calls

Switch statements are formatted like ordinary function calls. For fallthrough cases (`case = ,`), an extra space is added after the `=` to mark the fallthrough.

```r
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
# Before formatting
data[ row,col ]
data[[ "name" ]]

# After formatting
data[row, col]
data[["name"]]
```

### Extract and namespace operators

**Extract and namespace operators** (`$`, `@`, `::`, `:::`) are formatted without spaces around them:

```r
# Before formatting
collection $ item
collection @ item
pkg :: process
pkg ::: filter

# After formatting
collection$item
collection@item
pkg::process
pkg:::filter
```

When an extract or namespace chain spans several lines, each subsequent line is indented one step:

```r
# Before formatting
object$
call(x)$
call(x, y)

# After formatting
object$
  call(x)$
  call(x, y)
```

### String literals

String literals are normalized to double quotes (`"`), unless the string contains unescaped double quotes:

```r
# Before formatting
message <- 'Hello world'
quoted_content <- 'Say "hello"'

# After formatting
message <- "Hello world"
quoted_content <- 'Say "hello"'
```

Multi-line string literals always keep their original indentation and line breaks, no matter where they appear. Even if surrounding code is refactored or deleted, the formatter never changes the internal content of multi-line strings.

```r
# Before formatting
# { <- parent block gets deleted
    x <- "This is a multi-line string.
          It preserves
          indentation and line breaks."
# }

# After formatting
# { <- parent block gets deleted
x <- "This is a multi-line string.
          It preserves
          indentation and line breaks."
# }
```

[Raw string literals](https://search.r-project.org/R/refmans/base/html/Quotes.html) (`r"(...)"`, `R"[...]"`, and their custom-dash variants) are preserved byte-for-byte. Quote normalization is never applied to them. Their whole purpose is to hold characters that would otherwise need escaping, including a quote and a backslash.

```r
# Before formatting
path <- r"(C:\Users\me)"
quoted <- r"(He said "hi")"

# After formatting
path <- r"(C:\Users\me)"
quoted <- r"(He said "hi")"
```

### R6 class definitions

Class definitions with empty lines between methods are preserved:

```r
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

### Comments

The formatter ensures that a space is inserted after the `#` for standard comments. For special comment types like Roxygen (`#'`) and plumber (`#*`) comments, the space is placed after the initial two characters:

```r
# Before formatting
# comment with space
#comment without space
#'roxygen comment
#*plumber comment
#'string' <- commented out string
#!/usr/bin/env Rscript

# After formatting
# comment with space
# comment without space
#' roxygen comment
#* plumber comment
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
# Before formatting
#:fn( x:integer,y:double )->character
render <- function(x, y) as.character(x + y)
#: list[ named : double ]
weights <- list(a = 1.0)
#: Either< E , A >|NULL
outcome <- NULL

# After formatting
#: fn(x: integer, y: double) -> character
render <- function(x, y) as.character(x + y)
#: list[named: double]
weights <- list(a = 1.0)
#: Either<E, A> | NULL
outcome <- NULL
```

Anything that does not parse as an annotation is left verbatim, beyond ensuring the single space after `#:`. Prose, a dotted type name, a `pkg::fun` reference, and a malformed type are all left alone, so the formatter never corrupts a comment it does not understand. Consecutive `#:` lines form one annotation block (exactly as the type checker groups them), so a block that is not a single valid annotation is also left as written. Two compact annotations with no blank line between them are one such block.

The reformat is deliberately non-invasive: token order, identifier casing, and your line breaks are preserved. A single-line annotation stays on one line, an expanded annotation (one written with `@param` / `@return` lines) keeps one directive per line rather than being collapsed into a compact `fn(...)`, and content lines are never rejoined.

```r
# Before formatting
#: @param count { integer }
#: @param render { fn(integer)->character }
#: @returns { character }

# After formatting
#: @param count {integer}
#: @param render {fn(integer) -> character}
#: @returns {character}
```

When an annotation wraps across several `#:` lines, indentation and closing brackets are normalized from the shape of the opening brackets. An opening bracket followed by more content on its own line is *hugged*: its closer stays glued to the token before it. An opening bracket that ends its line is *expanded*: its closer gets a line of its own, aligned with the line that opened it. Each line is indented one step (`indent-width` spaces) per enclosing expanded bracket. Both canonical styles are stable:

```r
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
# Before formatting
#: @type Future {
#:   list{
#:     instrument: Instrument,
#:     maturity: integer
#: }}

# After formatting
#: @type Future {
#:   list{
#:     instrument: Instrument,
#:     maturity: integer
#:   }
#: }
```

A blank line, a non-`#:` comment, or ordinary code ends an annotation block, so unrelated comments are never pulled into one. Trailing empty `#:` lines at the end of a block are dropped.

### Line spacing

The formatter normalizes line spacing between expressions, allowing at most one empty line:

```r
# Before formatting
x <- 1
y <- 2


z <- 3

# After formatting
x <- 1
y <- 2

z <- 3
```

### Line endings

The formatter automatically detects and preserves the line ending style (`LF` or `CRLF`) used in the original file.

## Format Suppression

You can disable formatting for specific code sections using the `# fmt: skip` comment directive. This is useful when you want to preserve specific formatting for readability, such as aligned data structures. The skipped expression is preserved byte-exactly, including the column of its first line, so hand-aligned constructs keep their alignment.

The `fmt: skip` directive can be placed before any expression to skip formatting for it:

```r
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

For a whole region, use `# fmt: off` and re-enable formatting with `# fmt: on`. The region between the directives is preserved byte-exactly, keeping its original indentation, columns, and blank lines. The directives work at the top level as well as inside functions and `{ ... }` blocks; a region left open runs to the end of its enclosing block.

```r
f <- function() {
  # fmt: off
    x  <-  c(1,   2,
             30,  4)
  # fmt: on
  x
}
```

You can also skip formatting for an entire file by placing `# fmt: skip-file` at the top of the file. This directive must be placed at the very beginning of the file to take effect.

## Rationale

This section records the design decisions behind the rules above.

### Auto-bracing

**Accidental bugs:** Omitting braces in loops, function definitions, or `if` expressions makes a later edit change what the code means. Adding a line after an unbraced `if` leaves only the first line under the condition:

```r
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
# Before formatting
if (condition)
  action()

# After formatting
if (condition) {
  action()
}
```

For a control flow structure such as a `for`, `while`, or `repeat` loop, the formatter always adds braces around the body, whatever its length, because a single-line loop is not allowed. See [Loops](#loops).

```r
# Before formatting
for (item in sequence)
  action()

# After formatting
for (item in sequence) {
  action()
}
```

### Hugging behavior

"Hugging" is the compact layout for a nested expression in a multiline context: the inner expression starts on the same line as the outer expression's opening delimiter. Both hugged and expanded forms are allowed, and the formatter preserves whichever was written.

**Nested function calls** can be formatted in a hugged style:

```r
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
(expression +
  other_part)

# Also allowed
(
  expression +
    other_part
)
```

See [compact multiline calls](#function-calls) under Function calls.
