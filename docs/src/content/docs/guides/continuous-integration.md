---
title: Continuous integration
description: A working CI job for ry, and how to decide what fails the build
---

ry is a single binary that needs no R installation, so a CI job is a download and two commands:

```yaml
# .github/workflows/ry.yml
name: ry
on: [push, pull_request]

jobs:
  ry:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install ry
        env:
          RY_VERSION: 0.3.1-beta
        run: |
          curl -sSL "https://github.com/felix-andreas/ry/releases/download/${RY_VERSION}/ry-x86_64-unknown-linux-gnu.tar.gz" \
            | tar xz
          sudo mv ry /usr/local/bin/
      - run: ry check
      - run: ry fmt --check
```

**Pin the version.** A newer release can report findings an older one did not, and an unpinned job
then fails on code nobody changed. Pinning is also required in practice: every release is marked a
pre-release, so GitHub's `releases/latest` still points at an old build.

## What fails the build

| Exit code | Meaning |
| --- | --- |
| `0` | no findings |
| `1` | findings: by default, warnings count too |
| `2` | ry could not run: invalid `ry.toml`, a missing path, an unreadable file |

Treat `2` separately from `1` if your CI reports results: it means the configuration is broken, not
the code.

Warnings fail the build so that a clean project stays clean. While a project still has a backlog of
warnings, gate on errors only:

```sh
ry check --min-severity error
```

Warnings below the floor are neither printed nor counted. Under [strict mode](/reference/type-system#strict-mode),
`unresolved` becomes an error and starts counting again.

## JSON output

`ry check --output json` writes one JSON object per finding to stdout and nothing else, for tools
that annotate pull requests:

```json
{"code":"type-mismatch","column":21,"endColumn":27,"endLine":4,"line":4,"message":"expected `integer`, found `character`","path":"/home/you/demo/main.R","related":[],"severity":"error"}
```

The field names are a stable contract, documented in the [CLI reference](/reference/cli#json-output).
