---
title: Installation
description: Every way to install ry, including the cases the quick path does not cover
---

The common paths are on the install section of
[getting started](/getting-started#install). This page covers the remaining cases.

## VS Code

The [ry extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry)
bundles the binary for **Linux x86_64, macOS aarch64, and Windows x86_64**.

On any other architecture the extension installs but has no binary to run. Install the CLI
separately and point the extension at it:

```json
// settings.json
{
  "ry.path": "/absolute/path/to/ry"
}
```

Every extension setting is listed under
[editor settings](/reference/configuration#editor-settings).

## Zed

Not in Zed's extension registry yet. Until it is, install it from the repository as a dev extension:

1. Install a [Rust toolchain](https://rustup.rs) — Zed compiles dev extensions to WebAssembly itself.
2. Clone the repository.
3. Run `zed: install dev extension` from the command palette and select the `editors/zed` directory.

Zed has no built-in R support, so install the [R extension](https://zed.dev/extensions/r) first. Full
instructions, including how the extension finds the binary, are in
[`editors/zed`](https://github.com/felix-andreas/ry/tree/main/editors/zed).

## Command line

**Prebuilt binary.** Download from
[Releases](https://github.com/felix-andreas/ry/releases). Assets are named by Rust target triple
and each archive holds a single `ry` binary:

| Platform | Asset |
| --- | --- |
| Linux x86_64 | `ry-x86_64-unknown-linux-gnu.tar.gz` |
| macOS aarch64 | `ry-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `ry-x86_64-pc-windows-gnu.zip` |

```bash
curl -sSL https://github.com/felix-andreas/ry/releases/download/0.3.1-beta/ry-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ry /usr/local/bin/
```

Name the tag explicitly. Every release since `0.1.1` is marked a pre-release, so `releases/latest/`
still resolves to `0.1.1` rather than to the newest build.

**From source**, if you have a [Rust toolchain](https://www.rust-lang.org/tools/install):

```bash
cargo install --git https://github.com/felix-andreas/ry ry-lang
```

The crate is `ry-lang`. The command it installs is `ry`. This is also the route for an architecture
with no prebuilt binary.

## RStudio

RStudio has no language-server integration, but it can use ry as its external formatter:

1. **Tools → Global Options → Code → Formatting**, set **Code formatter** to **External**, then set
   **Reformat command** to `path/to/ry fmt`. RStudio appends the file name to that command.
2. For format-on-save, **Tools → Global Options → Code → Saving** and tick **Reformat documents on
   save**.

Type checking and code analysis are not available inside RStudio. Run `ry check` in a terminal, or
in [CI](/guides/continuous-integration).

## Verifying

```bash
ry --version
```

Then run it on a project — [getting started](/getting-started) shows what a first run looks like.
