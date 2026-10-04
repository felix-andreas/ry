---
title: Installation
description: Every way to install ry, including the cases the quick path does not cover
---

## VS Code

The [ry extension](https://marketplace.visualstudio.com/items?itemName=felix-andreas.ry)
bundles the binary for **Linux x86_64, macOS aarch64, and Windows x86_64**.

On any other architecture the extension installs fine but has no binary to run, so install the CLI
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

The Zed extension is not in Zed's registry yet. Until it is, install it from the repository as a
dev extension:

1. Install a [Rust toolchain](https://rustup.rs), which Zed uses to compile dev extensions to
   WebAssembly.
2. Clone the repository.
3. Run `zed: install dev extension` from the command palette and select the `editors/zed` directory.

Zed has no built-in R support, so install the [R extension](https://zed.dev/extensions/r) first. Full
instructions, including how the extension finds the binary, are in
[`editors/zed`](https://github.com/felix-andreas/ry/tree/main/editors/zed).

## Command line

**Prebuilt binary.** Download one from [Releases](https://github.com/felix-andreas/ry/releases).
Each asset is named after its Rust target triple, and each archive holds a single `ry` binary:

| Platform | Asset |
| --- | --- |
| Linux x86_64 | `ry-x86_64-unknown-linux-gnu.tar.gz` |
| macOS aarch64 | `ry-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `ry-x86_64-pc-windows-gnu.zip` |

```bash
curl -sSL https://github.com/felix-andreas/ry/releases/download/0.3.1-beta/ry-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv ry /usr/local/bin/
```

Name the tag explicitly: every release since `0.1.1` is marked a pre-release, so `releases/latest/`
still points at `0.1.1`.

**From source**, if you have a [Rust toolchain](https://www.rust-lang.org/tools/install):

```bash
cargo install --git https://github.com/felix-andreas/ry ry-lang
```

The crate is `ry-lang`, and the command it installs is `ry`. This also works on platforms without a
prebuilt binary.

## RStudio

RStudio has no language-server integration, but it can use ry as its external formatter:

1. Open **Tools → Global Options → Code → Formatting**, set **Code formatter** to **External**, and
   set **Reformat command** to `path/to/ry fmt`. RStudio appends the file name to that command.
2. For format-on-save, open **Tools → Global Options → Code → Saving** and tick **Reformat documents
   on save**.

Diagnostics do not show up inside RStudio; run `ry check` in a terminal or in
[CI](/guides/continuous-integration).
