---
title: Functions
description: Function signatures, optional parameters, and generic functions
---

## Signatures

```r
#: fn(name: character, [greeting]: character, ...: character) -> character
greet <- function(name, greeting = "hello", ...) paste(greeting, name, ...)
```

Calls are checked for types, names, and the number of arguments. Parameter names are part of the
type, because R matches arguments by name as often as by position:

```r
greet("Ada", greting = "hi")
```

```text
type-mismatch

  x this function has no parameter `greting`. Did you mean `greeting`?
```

`[greeting]` marks a parameter callers may omit, and the definition must give it a default, so an
omitted argument always has a value. `...: character` checks every extra argument.

## One signature per function

Your own functions have exactly one signature. To accept several shapes, use a union such as
`integer | character`. Several signatures per name would make every call a search over candidates,
and would leave the function without one type to pass around as a value. The shipped declarations
overload a few base functions such as `sum`, where the candidates are written out and the search
stays small.

## Generic functions

A function is as general as its body allows. Hover shows the inferred types:

```r
identity2 <- function(x) x           # <T> fn(x: T) -> T
increment <- function(x) x + 1L      # <T: numeric> fn(x: T) -> T
```

`T` stands for any type, and `T: numeric` for `integer` or `double`, so `increment(2.5)` is a
`double` and `increment("a")` is an error. You can write the same in an annotation:

```r
#: <T> fn(condition: logical, value: T) -> T | NULL
```
