---
title: Continuous integration
description: A working CI job for ry, and how to decide what fails the build
---

ry needs no R installation, so a CI job is a download and two commands:

```yaml
# .github/workflows/ry.yml
name: ry
on: [push, pull_request]

jobs:
  ry:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - name: Install ry
        env:
          RY_VERSION: 0.3.1-beta
        run: |
          curl -fsSL "https://github.com/felix-andreas/ry/releases/download/${RY_VERSION}/ry-x86_64-unknown-linux-gnu.tar.gz" \
            | tar xz
          sudo mv ry /usr/local/bin/
      - run: ry check
      - run: ry fmt --check
        if: always()
```

**Pin the version.** A newer release can report findings an older one did not, and an unpinned job
then fails on code nobody changed. Pinning is also required in practice: every release is marked a
pre-release, so GitHub's `releases/latest` still points at an old build.

`if: always()` runs the format check even when `ry check` fails, so one run shows both kinds of
problem.

## What fails the build

| Exit code | Meaning |
| --- | --- |
| `0` | no findings |
| `1` | findings: by default, warnings count too |
| `2` | ry could not run: invalid TOML in `ry.toml`, a missing path, an unreadable file |

A misspelled key in `ry.toml` is not exit `2`. ry prints a warning on stderr and ignores the key, so
`typng = true` leaves type checking off without failing the build. Read the job's log once after
changing the configuration.

Warnings fail the build so that a clean project stays clean. While a project still has a backlog of
warnings, you can gate on errors only:

```sh
ry check --min-severity error
```

Warnings below the floor are neither printed nor counted. Know what that leaves: syntax errors, type
errors (with `typing = true`), `trailing-comma`, and `NAMESPACE` entries that would stop the package
from loading. Unresolved and unused names are warnings, so with typing off this gate checks little
more than syntax. Under [strict mode](/reference/type-system#strict-mode), `unresolved` becomes an
error and counts again. Silencing one noisy class of finding, such as `unused = false` under
`[check]`, usually keeps more checking than an error-only gate.

## Annotations on pull requests

`ry check --output json` writes one JSON object per finding to stdout and nothing else:

```json
{"code":"type-mismatch","column":21,"endColumn":27,"endLine":4,"line":4,"message":"expected `integer`, found `character`","path":"/home/you/demo/main.R","related":[],"severity":"error"}
```

On GitHub, one `jq` line turns that into annotations on the pull request's diff:

```yaml
      - run: |
          ry check --output json | jq -r '"::\(.severity) file=\(.path | ltrimstr(env.GITHUB_WORKSPACE + "/")),line=\(.line),col=\(.column),title=\(.code)::\(.message)"'
          exit "${PIPESTATUS[0]}"
```

The field names are a stable contract, documented in the [CLI reference](/reference/cli#json-output).
