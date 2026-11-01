---
title: Formatting
description: What ry fmt does, and why it never reflows your lines
---

`ry fmt` gives every file in the project the same spacing, indentation and braces, so a review shows
what changed instead of whose style it is. Run it in the project directory to format files in place:

```r
x<-c(1,2,3)
if(x>1){y<-2}
for (i in 1:3) print(i)
```

```text
$ ry fmt
1 file reformatted, 0 files left unchanged
```

```r
x <- c(1, 2, 3)
if (x > 1) { y <- 2 }
for (i in 1:3) {
  print(i)
}
```

Every loop body, and every `if` or function body that spans lines, gets braces, so a line added
later cannot fall outside the block it belongs to. Your editor formats with the same rules
([Editors](/guides/editors)).

## Your line breaks decide the layout

`ry fmt` never reflows code to a column limit, because a formatter that does rewrites lines you did
not touch and buries your change in layout churn. A call written on one line stays on one line, and
a call you broke across lines gets one argument per line:

```r
report <- summarise(sales, total = sum(amount), n = n())
report <- summarise(sales,
  total = sum(amount), n = n())
```

```r
report <- summarise(sales, total = sum(amount), n = n())
report <- summarise(
  sales,
  total = sum(amount),
  n = n()
)
```

## In CI

`ry fmt --check` changes nothing. It lists the files it would change and exits with status 1 if
there are any:

```text
$ ry fmt --check
Would reformat: ./R/report.R
1 file would be reformatted, 0 files already formatted
```

The only settings are indent width and line endings, in [`ry.toml`](/reference/configuration).
[Formatting rules](/reference/formatting-rules) has every rule with examples.
