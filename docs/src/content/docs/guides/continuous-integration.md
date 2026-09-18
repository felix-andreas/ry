---
title: Continuous integration
description: A working CI job for R, and how to decide what should fail the build
---

ry is one binary with no R dependency, so a CI job is a download and two commands. Nothing to
install, no package cache, no matrix over R versions.

## A working job

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
      - name: Check
        run: ry check
      - name: Format
        run: ry fmt --check
```

Both commands exit `1` on findings, which is what fails the job. Neither needs R installed.

**Pin the version.** Every release since `0.1.1` is marked a pre-release, so
`releases/latest/download/…` resolves back to `0.1.1` rather than to the newest build. The type
system is also still gaining capability, so a newer version can report findings an older one did
not. Name the tag explicitly, as above. See [project status](/why-ry#project-status).

Asset names follow the Rust target triple, as in `ry-aarch64-apple-darwin.tar.gz` and
`ry-x86_64-pc-windows-gnu.zip`. Each archive contains the single `ry` binary.

## Deciding what should fail the build

The default is strict: **warnings fail the job**. A run with nothing but `unused` warnings still
exits `1`.

That is usually right for a project that starts clean, and wrong for one adopting ry on an existing
codebase. To gate on errors only while you work through a backlog:

```bash
ry check --min-severity error
```

The filter applies before anything is reported, so warnings are neither printed nor counted toward
the exit code. A run whose only findings are warnings prints `1 file checked, no problems` and exits
`0`.

| You want | Command |
| --- | --- |
| Everything to matter | `ry check` |
| Only errors to block the build | `ry check --min-severity error` |
| Formatting enforced | `ry fmt --check` |
| To see the diff CI would apply | `ry fmt --diff` |

Exit code `2` means the run could not be completed: an unparseable `ry.toml`, a path that does not
exist, or an unreadable file. It is a different failure from `1`, not a worse one. A job that
treats any non-zero status as "findings" will report a broken config as a code problem. The full
table is in the [CLI reference](/reference/cli#exit-codes).

## JSON output

```bash
ry check --output json
```

writes JSON Lines to stdout: one object per finding, nothing else on the stream, and no summary
line. In JSON mode no diagnostic is rendered to stderr. Configuration warnings and the exit-2 failures
below still go there, so stderr is not always empty.

```json
{"code":"type-mismatch","column":21,"endColumn":27,"endLine":4,"line":4,"message":"expected `integer`, found `character`","path":"/home/you/demo/main.R","related":[],"severity":"error"}
```

Every field is documented in the [CLI reference](/reference/cli#json-output), and the field names
are a contract.

Counting errors for a summary line, without jq:

```bash
ry check --output json | grep -c '"severity":"error"'
```

## Adopting on an existing project

Do not start by putting `ry check` in front of a merge gate on a codebase that has never run it.
Land the tool first, gate second. [Adopting an existing codebase](/guides/adopting) walks through
the order that works.
