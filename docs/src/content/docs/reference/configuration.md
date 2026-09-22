---
title: Configuration
description: Every ry.toml key, discovery rule, and editor setting in one place
---

Everything you can change about ry's behavior lives in one file, `ry.toml`. Editor settings only say where the binary is.

## Project discovery

`ry.toml` is the configuration file. There is no home-directory config, no environment variable naming one, and no merging. The nearest file replaces the built-in defaults wholesale.

| Where ry runs | Search starts at |
| --- | --- |
| `ry check R/utils.R` | the file's own directory |
| `ry check .` | that directory |
| The language server | the workspace folder your editor announces; failing that, the process working directory |

| Rule | Behavior |
| --- | --- |
| Search | walk up from the starting directory; the first `ry.toml` wins. None found: built-in defaults. |
| Merging | None. One file supplies every key. |
| Reload | the language server watches `ry.toml` and re-discovers on every change, so deleting it falls back to an ancestor or to the defaults. |
| Several CLI targets | discovery runs once per argument, so two arguments can resolve two different files. |
| `..` in a path | cancelled textually before the search, so `project/ry.toml` does **not** govern `project/../outside.R`. |

```console
$ cat project/ry.toml
spaces = 8
$ ry fmt --diff project/inside.R
Diff in project/inside.R:
1   1    | f <- function(x) {
2        |-  x
    2    |+        x
3   3    | }
1 file would be reformatted, 0 files already formatted
$ ry fmt --diff project/../outside.R
0 files would be reformatted, 1 file already formatted
```

### Project root

The project root is a separate question from which configuration applies. It sets the analysis scope, meaning which files see each other's definitions:

| Situation | Root |
| --- | --- |
| An ancestor holds `ry.toml` or `DESCRIPTION` | the nearest such directory |
| Otherwise, the target is a directory | that directory |
| Otherwise, the target sits directly under an `R/` directory | the parent of `R/` |
| Otherwise | the file's own directory |

## `[format]`

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `indent-width` | integer | `2` | Spaces per indentation level, for `ry fmt` and for formatting in the editor. |
| `line-ending` | `"auto"`, `"lf"`, `"cr-lf"` | `"auto"` | Line ending the formatter writes. `"auto"` keeps whatever the file already uses. |

## `[lint]`

Every key except `naming-style` takes a level: `"off"`, `"warn"`, `"error"`, or `"default"`. The last one means the built-in severity, exactly as if you omitted the key.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `naming-style` | `"snake_case"`, `"camelCase"` | unset, so the check is off | Reports `naming-style` for variables and function parameters that do not match. `SCREAMING_SNAKE_CASE` always conforms. Always a warning; the value is a style, not a level. |
| `assignment-operator` | level | `"warn"` | `=` used for assignment. |
| `boolean-shorthand` | level | `"warn"` | `T` or `F` written instead of `TRUE` or `FALSE`. |
| `trailing-comma` | level | `"error"` | A comma after the last argument of a call. |
| `unused-parameter` | level | `"off"` | Function formals never read. S3 methods and your project's own generics are exempt. |
| `unused-import` | level | `"off"` | An `importFrom(pkg, name)` in `NAMESPACE` whose name appears nowhere in your sources. Whole-namespace `import(pkg)` is never checked, and `ry check` raises this finding, while the editor does not. |
| `shadows-builtin` | level | `"off"` | A top-level binding with the same name as a `base` export. |
| `shadows-namespace` | level | `"off"` | A top-level binding with the same name as an export of another namespace, such as `stats::filter`. |

For a single exception, prefer a [suppression comment](/reference/diagnostic-codes#suppressing-a-finding) over turning a lint off across the whole project.

## `[check]`

Type inference always runs, so hover, inlay hints, and signature help work whatever these keys say. `[check]` only decides which findings are reported.

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `unused` | boolean | `true` | Report `unused`, which is a binding whose value is never read. |
| `typing` | boolean | `false` | Report `type-mismatch`. See the [tutorial](/type-checking/tutorial). |
| `maybe-undefined` | boolean | `false` | Report `maybe-undefined`, which is a read some path reaches with no prior write. Off by default because correlated guards read as independent branches; see [diagnostic codes](/reference/diagnostic-codes). |
| `strict` | boolean | `false` | Report each site with a genuinely undetermined type, **and** raise every `unresolved` finding from warning to error. See [strict mode](/reference/type-system#strict-mode). |
| `exclude` | array of strings | `[]` | Gitignore-style patterns the directory walk of `ry check` skips. |

A `# typing: off`, `# typing: on`, or `# typing: strict` line at the top of a file replaces both `typing` and `strict` for that file. See [the per-file directive](/reference/type-system#per-file-directive).

The `exclude` patterns work like this:

- Patterns are anchored at the directory holding `ry.toml`, and follow gitignore rules: `scripts/` excludes that whole subtree, `**/generated` matches at any depth, `!` re-includes.
- Excluded directories are pruned without being walked, so exclusion cuts checking time, not just output.
- A file named on the command line is always checked, files open in the editor are always analyzed, and `ry fmt` ignores the key entirely.
- Some paths are skipped with no configuration at all, because they hold vendored dependencies rather than your code: `renv/`, `packrat/`, `revdep/`, `.Rproj.user/`, `.Rcheck/`. `.gitignore` is honored too, git checkout or not.

```console
$ cat ry.toml
[check]
exclude = ["scripts/"]
$ ry check .
unused

  ! `v` is assigned but never used.
   --[R/a.R:1:19]
 1 | f <- function() { v <- 1; 2 }
   |                   ^

1 problem in 1 file
```

## Invalid and unknown keys

An unknown key is never fatal, so a config written for a newer ry still starts an older one.

| Situation | Result |
| --- | --- |
| Unknown key | Ignored, with one warning naming it. Known keys beside it still load, and the exit code is unaffected. |
| Known key at the wrong level | Ignored, with a warning naming the table it belongs under. A `typing = true` written outside `[check]` sets nothing. |
| Wrong type on a known key | Hard error. |
| Malformed TOML | Hard error. |
| Invalid `[check] exclude` pattern | Hard error. |
| The file disappears between discovery and reading | Silently falls back to the defaults. |
| Any other read error | Hard error. |

```console
$ cat ry.toml
strict = true

[check]
stric = true
typing = true

[format]
indent = 4
$ ry check .
  ! ignoring config key `strict`. It belongs under `[check]`, and nothing outside a table sets it
  ! ignoring unknown config key `check.stric`. Check the spelling, or update ry
  ! ignoring unknown config key `format.indent`. Check the spelling, or update ry
1 file checked, no problems
```

A hard error shows the offending line:

```console
$ cat ry.toml
[check]
typing = "yes"
$ ry check .
config

  x invalid config for `check.typing`: invalid type: string "yes", expected a boolean
   --[/home/you/project/ry.toml:2:10]
 1 | [check]
 2 | typing = "yes"
   |          ^^^^^
$ echo $?
2
```

Where that lands depends on how ry runs:

| | Behavior |
| --- | --- |
| CLI | The message goes to stderr and the command exits 2. See the [exit codes](/reference/cli#exit-codes). |
| The language server | Never crashes. At startup it falls back to the defaults; on a live edit it keeps the previous configuration. Either way it shows the message and publishes a `config` finding on `ry.toml` at the offending line, cleared once the file loads again. |

## Legacy keys

| Old key | Modern key | Note |
| --- | --- | --- |
| `case` (top level) | `lint.naming-style` | Still parses, and wins when both are set. |
| `spaces` (top level) | `format.indent-width` | Still parses, and wins when both are set. |
| `lint.missing-comma` | none | Accepted so old files keep loading, and does nothing: a missing argument comma is now a parse error. |

## Editor settings

These say where the binary is and how to launch it; none of them changes analysis. The language server ignores LSP workspace configuration outright, so every behavioral key must live in `ry.toml`.

### VS Code

| Setting | Default | Effect |
| --- | --- | --- |
| `ry.path` | `null` | Location of the `ry` executable. |
| `ry.args` | `null`, meaning `["server"]` | Arguments passed to the executable. |
| `ry.experimentalFeatures` | `null` | Feature names forwarded as `--experimental-features`. The only one today is `range_formatting`, which formats the selected range instead of the whole file. |

Changing any of the three prompts you to restart the server; it takes effect only then. The extension finds the binary in this order: the `SERVER_PATH` environment variable, `ry.path`, its own bundled copy, then `ry` on your `PATH`.

| Command | Does |
| --- | --- |
| ry: Restart Server | Restarts the language server |
| ry: Start Server | Same as restart |
| ry: Stop Server | Shuts the language server down |
| ry: Open Logs | Opens the server's output channel |

### Zed

The extension contributes no settings of its own, so use Zed's generic `lsp.ry` block.

| Setting | Default | Effect |
| --- | --- | --- |
| `lsp.ry.binary.path` | unset | Absolute path to the binary. Setting it skips the `PATH` lookup and the release download. |
| `lsp.ry.binary.arguments` | unset, meaning `["server", "--stdio"]` | Arguments passed to the binary, however it was found. |
| `lsp.ry.settings` | unset | Forwarded to the server, which ignores it. Put configuration in `ry.toml`. |

Zed highlights R with tree-sitter, which sees a `#:` annotation as an ordinary comment. The colors come from the server as semantic tokens, which Zed leaves off by default:

```json
// settings.json
{
  "languages": {
    "R": {
      "semantic_tokens": "combined"
    }
  }
}
```

Without a path or a binary on your `PATH`, the Zed extension downloads a release itself and reuses it afterwards.
