# A `#:` line is a comment to R whatever it holds, so text no lexer accepts there stops nothing.
#: fn(x: 'abc) -> I64
f <- function(x) x
print(f(1))
#: fn(x: 0x) -> I64
g <- function(x) x + 1
print(g(1))
#: fn(x: 1e, y: "\q") -> ``
h <- function(x, y) x + y
print(h(1, 2))
x <- 3 #: `a\0b` "unclosed
print(x)
# A backslash before the line break does not carry the comment's quote onto the next line.
#: "a\
print(4) # "
#: 'b\
y <- 5 # '
print(y)
texts <- c(
  "#: 'abc\n1", "#: 0x\n2", "#: 1e5L 0x1.p\n3", "#: ``\n4", "#: '\\q' \"\\400\"\n5", "6 #: `",
  "#: r\"(\n7", "#: %\n8", "#: \\\n9", "#: 1 ;; 2 |> 3 _\n10", "#: \"a\\\n11 # \"", "#: `a\\\r\n12 # `"
)
for (text in texts) {
  cat(encodeString(text), "=>", deparse(parse(text = text, keep.source = FALSE)), "\n")
}
