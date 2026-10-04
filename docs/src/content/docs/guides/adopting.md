---
title: Adopting an existing codebase
description: The order that turns ry on for an existing R project without burying you in findings
---

Turning everything on at once on a project that has never been checked produces a long list, and a
long list gets ignored. This order keeps each step small enough to finish.

## 1. Names first

Run `ry check` with no `ry.toml`. Unresolved names, unused assignments, and duplicate definitions
need no understanding of the type system, are almost always real, and are usually a short list. Fix
them, then gate CI on them with `ry check` so they stay fixed.

## 2. Turn on types, and read before fixing

```toml
[check]
typing = true
```

Read the whole list before fixing anything. On real code most findings fall into a few shapes, and
recognizing the shape is worth more than fixing the first instance:

| Finding | Usually means |
| --- | --- |
| A value that may be `NULL`, used unguarded | A missing `is.null()` check, or a value that is never `NULL` for a reason ry cannot see. Either guard it or annotate the source |
| A field that does not exist | A typo, or a field added by code ry did not follow. The message shows the fields ry did see |
| Calling something that is not a function | A variable that shadows a function name |

Type mismatches are errors, so committing `typing = true` with findings left would fail CI. Run it
locally until the list is short, or turn typing on file by file instead.

## 3. Go file by file

A comment at the top of a file overrides the project setting in either direction:

```r
# typing: on       # check this file while the project default is off
# typing: off      # skip this file for now
```

Start with the files you change most, because that is where a type error costs most. Leave typing
off project-wide and opt files in, or turn it on and opt the hard ones out.

## 4. Strict mode where it counts

A clean run with `typing = true` means no contradictions were found. It does not mean the file was
checked: a data-frame-heavy file can pass while barely being checked at all, because column reads
are `Unknown`. Strict mode reports every such place, and it raises `unresolved` to an error. Use it
on the modules you rely on most (`# typing: strict` at the top), not across a legacy codebase.

## Where annotations pay off

Inference covers function bodies. Annotate the boundaries: exported functions, constructors, and
values read from outside the program, such as `readRDS()` results, which are `Any` until you say what
they contain.
