---
title: Releasing
description: How a ry release is cut, what it ships, and the checklist to follow
---

A release is one tag. Pushing a tag such as `0.4.0-beta` (no leading `v`), or starting the Release
workflow by hand with that version, builds every platform's archive on its own runner, smoke-tests
it, and publishes the GitHub release and the VS Code extension. The workflow is
`.github/workflows/release.yml`; until a maintainer moves it there, it waits in
`.github/pending-release.yml`, and there is no other release path.

## What a release ships

Each archive holds `ry` and ry's runtime side by side, so `ry run` finds the runtime without
configuration:

| Archive | Contents |
| --- | --- |
| `ry-x86_64-unknown-linux-gnu.tar.gz` | `ry`, `libR.so`, `LICENSE` |
| `ry-aarch64-unknown-linux-gnu.tar.gz` | `ry`, `libR.so`, `LICENSE` |
| `ry-aarch64-apple-darwin.tar.gz` | `ry`, `libR.dylib`, `LICENSE` |
| `ry-x86_64-apple-darwin.tar.gz` | `ry`, `libR.dylib`, `LICENSE` |
| `ry-x86_64-pc-windows-gnullvm.zip` | `ry.exe`, `R.dll`, `LICENSE` |

ry looks for its runtime beside the executable and in `../lib/ry/`, so an installer may place
`ry` in `bin/` and the library in `lib/ry/`. At run time, `ry run`, `ry R` and `ry load` also need
a GNU R installation built as a shared library, because ry's runtime reads base R from it. `ry
check`, `ry fmt` and `ry server` need nothing else.

The Linux archives are built on Ubuntu 22.04, so they need glibc 2.35 or newer. On macOS, `libR.dylib`
has the install name `@rpath/libR.dylib` and exports the same names as the Linux library, which is
checked by cross-compiling from Linux (`contributing/design/r-runtime.md`, section 11). The smoke
test's `RY_SMOKE_PACKAGES` step, which loads a CRAN binary of jsonlite under `ry run`, is the first
run on a real Mac.

The Windows archive is built for `x86_64-pc-windows-gnullvm` with MSYS2's clang64 toolchain, so
`ry.exe` and `R.dll` link the UCRT, as Rtools-built packages do. Both have been run under Wine only
(`contributing/design/r-runtime.md`, section 11); the smoke test, which also loads a CRAN binary of
jsonlite and calls the installation's `Rblas.dll` and `Rlapack.dll`, is the first run on Windows.

The VS Code extension is published once per platform, each copy bundling that platform's `ry`,
plus one copy without a binary for every other platform. The Zed extension ships no binary: it
downloads the matching archive from the newest GitHub pre-release (`pre_release: true` in
`editors/zed/src/lib.rs`, which matches pre-releases only), and it is published to Zed's registry
separately. Flip that flag in the same change as the first stable release, or Zed keeps serving the
last beta.

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
7. Update [Installation](/installation). For the first release from the workflow that means: each
   archive holds `ry` and the runtime, which must stay side by side (or the runtime goes in
   `../lib/ry/`); the Windows archive is now `ry-x86_64-pc-windows-gnullvm.zip` and carries `R.dll`, so `ry run` and `ry load` work on Windows; Linux needs glibc
   2.35; and Linux arm64 and Intel macOS archives exist.
