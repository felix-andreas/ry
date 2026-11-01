---
title: Releasing
description: How a ry release is cut, what it ships, and the checklist to follow
---

A release is one tag. Pushing a tag such as `0.4.0-beta` (no leading `v`), or starting the Release
workflow by hand with that version, builds every platform's archive on its own runner, smoke-tests
it, and publishes the GitHub release and the VS Code extension. The workflow is
`.github/workflows/release.yml`; until a maintainer moves it there, it waits in `.github/pending-release.yml`.

## What a release ships

Each archive holds `ry` and ry's runtime side by side, so `ry run` finds the runtime without
configuration:

| Archive | Contents |
| --- | --- |
| `ry-x86_64-unknown-linux-gnu.tar.gz` | `ry`, `libR.so`, `LICENSE` |
| `ry-aarch64-unknown-linux-gnu.tar.gz` | `ry`, `libR.so`, `LICENSE` |
| `ry-aarch64-apple-darwin.tar.gz` | `ry`, `libR.dylib`, `LICENSE` |
| `ry-x86_64-apple-darwin.tar.gz` | `ry`, `libR.dylib`, `LICENSE` |
| `ry-x86_64-pc-windows-msvc.zip` | `ry.exe`, `LICENSE` (no runtime yet) |

ry looks for its runtime beside the executable and in `../lib/ry/`, so an installer may place
`ry` in `bin/` and the library in `lib/ry/`. At run time, `ry run`, `ry R` and `ry load` also need
a GNU R installation built as a shared library, because ry's runtime reads base R from it. `ry
check`, `ry fmt` and `ry server` need nothing else.

The VS Code extension is published once per platform, each copy bundling that platform's `ry`,
plus one copy without a binary for every other platform. The Zed extension ships no binary: it
downloads the matching archive from the latest GitHub release, and it is published to Zed's
registry separately.

## Version numbers

`just bump-version X.Y.Z-beta` sets `workspace.package.version` in `Cargo.toml` and the VS Code
extension's version, which drops the suffix because the Marketplace rejects one. The patch number is
therefore the one counter across channels: never release `0.4.0-beta` and `0.4.0` both. A
`-alpha` or `-beta` suffix marks a pre-release, on GitHub and on the Marketplace alike
(`just release-kind`). The Zed extension has its own version in `editors/zed/extension.toml` and
`editors/zed/Cargo.toml`, bumped by hand when the extension changes.

## Checklist

1. On a clean main, run `just release-dry-run`. It packages this machine's archive, installs it
   into a scratch directory, smoke-tests it (`tools/smoke-test.sh`), packages the VS Code extension
   and builds the nix package.
2. Run `just try` and read every step's output.
3. Write the release's entry on the [news page](/news) and move `[Unreleased]` in `CHANGELOG.md`
   under the new version.
4. Run `just bump-version X.Y.Z-beta` (it commits), and push.
5. Push the tag, or start the Release workflow with the version. The workflow refuses a version that
   differs from `Cargo.toml`.
6. Check the release page: one archive per platform and the `.vsix` files. Install one archive by
   hand and run `ry --version`.
7. Update [Installation](/installation) if a download name or a requirement changed.
