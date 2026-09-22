---
title: Adopting an existing codebase
description: How to turn type checking on for a project that has never had it, without being buried in findings
---

Setting `typing = true` on a codebase that has never been type-checked will report findings,
possibly a lot of them. Turn it on in stages instead.

## 1. Start with no configuration at all

Code analysis needs no opt-in. Run `ry check` with no `ry.toml` and read what comes back.
Unresolved names, unused bindings, and duplicate definitions find real mistakes on day one, and none
of them require you to understand the type system.

Fix those first. It is a short list on most projects, and it clears away the noise before you add
more.

## 2. Turn typing on

```toml
# ry.toml
[check]
typing = true
```

Do not fix anything on the first pass. Read the whole list instead. On real code it collapses into a
handful of shapes, and recognizing the shape is worth more than fixing the first instance:

| Shape | What it usually means |
| --- | --- |
| A value that may be `NULL` used without a guard | Genuine. Either the guard is missing, or the value can never actually be `NULL` and the checker cannot see why |
| A field read that no assignment created | `x$never_set` after only `x$a <- 1L`. The message names the record the checker built from the assignments it saw |
| A value inferred as something other than a function, being called | Usually a name that shadows a function, or a dynamic construction the checker cannot follow |

None of these mean your code is wrong. They mean the checker could not establish that it is right,
which is a different claim.

## 3. Move file by file

You do not have to fix the whole project to benefit from any of it. A directive at the top of a file
overrides the project setting, in any direction:

```r
# typing: on       # check this file even if the project has typing off
# typing: off      # skip this one while you work through the rest
# typing: strict   # hold this module to the stronger standard
```

If a file has more than one, the last directive wins, and any other value is an error.

So you can leave `typing = false` project-wide and opt in the modules you are actively working on,
or turn it on project-wide and exempt the files you are not ready for. Either way, start with the
files you change most often.

## 4. Strict mode

```toml
[check]
typing = true
strict = true
```

Strict mode makes a stronger claim than `typing = true`. It reports every place a value became
`Unknown`, meaning every point where the checker gave up instead of concluding something. It also
raises `unresolved` from a warning to an error, so `--min-severity error` no longer filters those
out of a CI run.

That is what you want on a module you intend to rely on, and not what you want across a whole legacy
codebase on day one. A clean ordinary run means the checker found no contradictions. A clean strict
run means it understood everything. Only the second is a guarantee.

Strict mode is also how you find out how much of a file is really being checked. Because `Unknown`
is compatible with everything, a data-frame-heavy file can pass cleanly while barely being checked
at all. See [limitations](/type-checking/limitations).

## Where the annotations go

Most code needs none. When you do need one, annotate at the **boundaries**: the exported function,
the constructor, and the code that reads from outside. Let inference cover the rest.
See [domain modeling](/type-checking/domain-modeling) for giving your own types names.
