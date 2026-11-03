---
title: Releasing
description: How a ry release is cut, what it ships, and the checklist to follow
---

A release is one tag. Pushing a tag such as `0.4.0-beta` (no leading `v`), or starting the Release
workflow by hand with that version, builds each platform's archive on its own runner, smoke-tests
it, and then publishes the GitHub release and the VS Code extension. The workflow is
`.github/workflows/release.yml`, and there is no other release path.

A run started by hand has two more inputs. `platforms` set to `linux` builds and releases only the
Linux archive, and unticking `vscode` leaves the VS Code extension out of the release and the
Marketplace. A tag push always releases every platform and the extension.

Every step of the workflow past runner setup is a `just` recipe (or `tools/smoke-test.sh`), so each
one runs locally as well:

| Recipe | What it does |
| --- | --- |
| `just release-check VERSION` | Fails unless `VERSION` is a valid release version and `Cargo.toml` and both extensions carry it. |
| `just package VERSION TARGET` | Builds one platform's archive into `release/VERSION`. |
| `just release-assets VERSION` | Unpacks the archives in `release/VERSION` and packages the VS Code extension from them. A platform without an archive is skipped with a note. |
| `just release-publish VERSION` | Creates the GitHub release with every archive and `.vsix` in `release/VERSION` (or refreshes the assets of one a failed run left behind), then publishes each `.vsix` to the Marketplace. |

`release-publish` needs `gh` signed in, and outside the workflow it needs a `VSCE_PAT`, because
without one it publishes with `--oidc`, which works only inside GitHub Actions.

## The workflows

`.github/workflows/` holds three workflows: `release.yml`, `docs.yml` (the docs deploy below) and
`ci.yml`. CI runs only when started by hand (from the Actions tab, on any ref), not on pushes or pull
requests. The orchestrating agent starts it on main at its hourly check when main has new commits.

Agents cannot write to `.github/workflows/`, so an agent that changes a workflow stages the new file
under the same name in `.github/pending/`. A maintainer then moves it into place, commits and
pushes with one command:

```bash
git pull && just accept
```

## Deploying the docs site

The docs site at https://ry-lang.org deploys only when someone asks for it, because the Cloudflare
Pages project's automatic deploys are off. To deploy, open **Actions**, then **Docs**, then **Run
workflow**, and enter a branch:

- `main` is a production deploy, which updates https://ry-lang.org.
- Any other branch is a preview, served at that branch's alias on the project's `pages.dev` domain
  (`<branch>.<project>.pages.dev`), while the live site stays as it is.

The workflow checks out the branch and runs `just docs-deploy BRANCH`, which builds `docs/` and
uploads the result with `wrangler pages deploy`. The recipe runs the same way from a checkout, with
the three variables below set in the environment; it fails, naming the missing ones, if any is
unset. It deploys whatever the checkout holds and labels the upload with its `HEAD` commit.

The workflow needs two repository secrets and one repository variable, all under the repository's
**Settings**, **Secrets and variables**, **Actions**:

- Secret `CLOUDFLARE_API_TOKEN`: a Cloudflare API token with the account permission **Cloudflare
  Pages: Edit**, created in the Cloudflare dashboard under **My Profile**, then **API Tokens**.
- Secret `CLOUDFLARE_ACCOUNT_ID`: the account ID shown on the account's overview page.
- Variable `CLOUDFLARE_PAGES_PROJECT`: the name of the Pages project behind ry-lang.org.

`main` is a production deploy only while it is the Pages project's production branch, so keep it
set there.

## Credentials

The GitHub release needs no secret: the workflow's own `GITHUB_TOKEN` creates it, with the
`contents: write` permission the publish job declares.

The VS Code Marketplace also needs no secret. The publish job uses trusted publishing: `vsce publish
--oidc` exchanges a GitHub OIDC token (the job's `id-token: write` permission) for a Marketplace
credential that lasts 15 minutes. The Marketplace accepts the exchange only from a workflow named in
a policy on the publisher, which a publisher owner creates once:

1. Sign in to the [publisher management page](https://marketplace.visualstudio.com/manage) and open
   the publisher `felix-andreas`.
2. Select **Trusted Publishing**, expand **Create**, and enter a policy name, the owner
   `felix-andreas`, the repository `ry`, and the workflow file `release.yml`. Leave the environment
   empty, because the job uses none.
3. Select **Create**.

A repository secret named `VSCE_PAT`, if one exists, takes precedence over trusted publishing. It is
a fallback for when the policy does not work. The Marketplace accepts only a token created for
**All accessible organizations**, and Azure DevOps retires such global tokens on December 1, 2026,
so the fallback stops working on that date. To create one, open
[Azure DevOps](https://dev.azure.com) with the account that owns the publisher, go to **User
settings**, then **Personal access tokens**, and select **New Token**. Choose **All accessible
organizations**, then **Custom defined** scopes, **Show all scopes**, and **Marketplace: Manage**.
Paste the token under the repository's **Settings**, **Secrets and variables**, **Actions**, as a new
repository secret named `VSCE_PAT`.

The extension is published to the VS Code Marketplace only, not to Open VSX. Positron and VSCodium
users install the `.vsix` files attached to the GitHub release.

## What a release ships

Each archive holds `ry` alone, with ry's runtime built into it, so `ry run` needs no
configuration, plus the licence and the third-party notices:

| Archive | Contents |
| --- | --- |
| `ry-x86_64-unknown-linux-gnu.tar.gz` | `ry`, `LICENSE`, `THIRD_PARTY_NOTICES` |
| `ry-aarch64-apple-darwin.tar.gz` | `ry`, `LICENSE`, `THIRD_PARTY_NOTICES` |
| `ry-x86_64-pc-windows-gnullvm.zip` | `ry.exe`, `LICENSE`, `THIRD_PARTY_NOTICES` |

`ry` is GPL-2.0-or-later ([licensing](/reference/licensing/)), and `LICENSE` points at the
release's tag as its source. `THIRD_PARTY_NOTICES` is committed and regenerated by
`just notices`. Run it after changing `Cargo.lock` or a GNU R provenance header, and run
`just notices --check` before a release, because `just package` copies the committed file as it is.

`just package` builds the runtime first and then `ry` with `RY_EMBED_RUNTIME` naming it, which
embeds it zstd-compressed (`crates/repl/build.rs`), along with `libR.dll.a` on Windows. On first
use `ry` extracts it into `ry/runtime/<hash>/` of the user's cache. `ry` is also the R front end
that `ry-rcmd` is in a development build: ry's R home runs it through a link named `ry-rcmd`
(on Windows, hard links named `R.exe`, `Rscript.exe` and so on), and `ry` acts as the front end
when started under such a name.

ry looks for its runtime in `RY_LIBR`, then beside the executable, then in `deps/` and
`../lib/ry/` next to it, and only then falls back to the built-in one, so an installer may still
place `ry` in `bin/` and the library in `lib/ry/`. At run time, `ry run`, `ry R` and `ry load` also
need a GNU R installation built as a shared library, because ry's runtime reads base R from it.
`ry check`, `ry fmt` and `ry server` need nothing else.

The Linux archive is built on Ubuntu 22.04, so it needs glibc 2.35 or newer. On macOS, `libR.dylib`
has the install name `@rpath/libR.dylib` and exports the same names as the Linux library, which is
checked by cross-compiling from Linux (`contributing/design/r-runtime.md`, section 11). The smoke
test's `RY_SMOKE_PACKAGES` step, which loads a CRAN binary of jsonlite under `ry run`, is the first
run on a real Mac.

The Windows archive is built for `x86_64-pc-windows-gnullvm` with MSYS2's clang64 toolchain, so
`ry.exe` and `R.dll` link the UCRT, as Rtools-built packages do. Both have been run under Wine only
(`contributing/design/r-runtime.md`, section 11); the smoke test, which also loads a CRAN binary of
jsonlite and calls the installation's `Rblas.dll` and `Rlapack.dll`, is the first run on Windows.

The VS Code extension is published once per platform, each copy bundling that platform's `ry`,
plus one copy without a binary (`ry.vsix`) for every other platform. Every copy carries a
`LICENSE` that combines the extension's MIT licence with ry's `LICENSE`, and
`THIRD_PARTY_NOTICES`. The Zed extension ships no binary: it downloads the matching archive from the newest
GitHub pre-release (`pre_release: true` in `editors/zed/src/lib.rs`, which matches pre-releases
only), and it is published to Zed's registry separately. Flip that flag in the same change as the
first stable release, or Zed keeps serving the last beta.

## Version numbers

`just bump-version X.Y.Z-beta` sets `workspace.package.version` in `Cargo.toml` to `X.Y.Z-beta`,
and the VS Code and Zed extensions' versions to `X.Y.Z`, because the Marketplace rejects a suffix.
The patch number is therefore the one counter across channels: never release `0.4.0-beta` and
`0.4.0` both. A `-alpha` or `-beta` suffix marks a pre-release, on GitHub and on the Marketplace
alike (`just release-kind`). `just release-check`, the release workflow's first step, refuses a
version that differs from any of the three.

## Checklist

1. On a clean main, run `just release-dry-run`. It packages this machine's archive, installs it into
   a scratch directory, smoke-tests it (`tools/smoke-test.sh`), packages the VS Code extension and
   builds the nix package.
2. Run `just try` and read every step's output.
3. Write the release's entry on the [news page](/news).
4. Run `just bump-version X.Y.Z-beta` (it commits), and push.
5. Dry-run the workflow: in the Actions tab, open **Release**, select **Run workflow**, enter the
   version, untick **publish**, and run it. It builds and smoke-tests every archive without
   publishing anything.
6. Push the tag (`git tag X.Y.Z-beta && git push origin X.Y.Z-beta`), or run the workflow again
   with **publish** ticked.
7. Check the release page: one archive per platform and the `.vsix` files. Install one archive by
   hand and run `ry --version`, and check the extension's version on the Marketplace.
8. Update [Installation](/installation) to name the new release and its archives. For the first
   release from the workflow that means: each archive holds `ry` alone, with the runtime built in,
   and the Windows archive is `ry-x86_64-pc-windows-gnullvm.zip` (say that `ry run` and `ry load`
   work on Windows only if the Windows smoke test passed).
