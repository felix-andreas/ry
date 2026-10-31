---
title: Formatting
description: What ry fmt does, and why it never reflows your lines
---

`ry fmt` formats files in place, and `ry fmt --check` only reports the files it would change. It
normalizes spacing, indentation, and braces:

```r
x<-c(1,2,3)                 # becomes  x <- c(1, 2, 3)
if(x>1){y<-2}               # becomes  if (x > 1) { y <- 2 }
```

It never reflows code to a column limit, because a formatter that does rewrites lines you did not
touch and buries your change in layout churn. Your line breaks decide the layout instead: a call
written on one line stays on one line, and a call broken across lines gets one argument per line.

Every loop body, and every `if` or function body that spans lines, gets braces, so a line added
later cannot fall outside the block it belongs to. The only settings are indent width and line
endings. [Formatting rules](/reference/formatting-rules) has every rule with examples.
