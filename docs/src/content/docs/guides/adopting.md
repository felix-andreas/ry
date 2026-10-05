---
title: Adopting an existing codebase
description: The order that turns ry on for an existing R project without burying you in findings
---

Turning everything on at once on a project that has never been checked produces a long list, and a
long list gets ignored. This order keeps each step small enough to finish.

## 1. Format in one commit

Run `ry fmt` over the whole project and commit the result on its own, with nothing else in it. Then
list that commit in `.git-blame-ignore-revs`, so blame skips it (GitHub reads the file on its own;
locally, set `git config blame.ignoreRevsFile .git-blame-ignore-revs`). Formatting first means every
later diff shows only real changes.

## 2. Names

Run `ry check` with no `ry.toml`. Unresolved names, unused assignments, and duplicate definitions
need no understanding of the type system, are almost always real, and are usually a short list. Fix
them, then gate CI on `ry check` so they stay fixed.

A clean result here can be misleading. If any file attaches a package that ry has no declarations
for, unresolved names are switched off for the whole project, because any name might be one of that
package's exports. Run once with `strict = true` under `[check]`: it lists every name that was let
through this way, and a [stub](/type-checking/stubs) for the package turns the check back on.

Generated or vendored code does not belong in the list at all. Skip it with `exclude` under
`[check]`.

## 3. Turn on types, and read before fixing

```toml
[check]
typing = true
```

Read the whole list before fixing anything. On real code most findings fall into a few shapes, and
recognizing the shape is worth more than fixing the first instance. A tally by code shows them:

```sh
ry check --output json | jq -r .code | sort | uniq -c | sort -rn
```

| Finding | Usually means |
| --- | --- |
| A value that may be `NULL`, used unguarded | A missing `is.null()` check, or a value that is never `NULL` for a reason ry cannot see. Guard it, or assert the type with `#: @trust TYPE` |
| A field that does not exist | A typo, or a field added by code ry did not follow. The message shows the fields ry did see |
| `expected integer, found double` in a call to your own unannotated function | A [known gap](/type-checking/limitations#where-correct-code-is-reported) in unannotated functions. Annotate the parameters as `double` |

Type mismatches are errors, so committing `typing = true` with findings left would fail CI. Run it
locally until the list is short, or turn typing on file by file instead.

## 4. Go file by file

A `# typing:` comment in a file overrides the project setting in either direction. `# typing: on`
checks the file while the project default is off, and `# typing: off` skips it while the default is
on:

```r
# typing: on
```

Start with the files you change most, because that is where a type error costs most. Leave typing
off project-wide and opt files in, or turn it on and opt the hard ones out. For a single finding you
have reviewed and want to keep, a `# ry: allow(code)` comment silences that line.

## 5. Strict mode where it counts

A clean run with `typing = true` means no contradictions were found. It does not mean the file was
checked: a data-frame-heavy file can pass while barely being checked at all, because column reads
are `Unknown`. Strict mode reports every such place, and it raises `unresolved` to an error. Use it
on the modules you rely on most (`# typing: strict`), not across a legacy codebase.

Values read from outside the program are where an annotation pays off most. A `readRDS()` result is
`Any` until you say what it contains, and saying so checks every use of it.
