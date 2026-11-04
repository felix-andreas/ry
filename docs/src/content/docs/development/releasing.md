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

## Platform tiers

Linux x86_64 is tier 1: its archive must pass the smoke test before a release is cut. macOS on Apple
silicon and Windows x86_64 are best effort. When either fails, start the workflow by hand with
`platforms` set to `linux`; the release then has Linux binaries only, its notes say so, and macOS and
Windows users stay on the last release that has their archive until a later release fixes the build.

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

`.github/workflows/` holds four workflows: `release.yml`, `nightly.yml` (below), `docs.yml` (the docs deploy below) and
`ci.yml`. CI runs only when started by hand (from the Actions tab, on any ref), not on pushes or pull
requests. The orchestrating agent starts it on main at its hourly check when main has new commits.

Agents cannot write to `.github/workflows/`, so an agent that changes a workflow stages the new file
under the same name in `.github/pending/`. A maintainer then moves it into place, commits and
pushes with one command:

```bash
git pull && just accept
```

## Nightly builds

A nightly is a Linux x86_64 build of any ref, started by hand: open **Actions**, then **Nightly**,
then **Run workflow**, and pick the branch. It builds and smoke-tests the archive, then publishes it as
a GitHub release named `nightly.<date>.<HHMM>.<sha6>`, from the commit's UTC committer date and time
and the first six hex digits of its hash (`just nightly-name` prints it, for example
`nightly.20261104.0712.999094`). `ry --version` prints the name after the version, because the
workflow sets `RY_BUILD`. The workflow does not run `release-check`, builds no editor extension, and
publishes nothing to the Marketplace.

A nightly is a normal release created with `--latest=false`, not a pre-release. The Zed extension
installs the newest release whose pre-release flag is set, so a pre-release nightly would reach Zed
users; a non-pre-release with `--latest=false` is invisible to it and leaves GitHub's "latest"
alone. The tag pattern of `release.yml` does not match `nightly.*` tags. `just nightly-publish NAME`
publishes `release/NAME` and then deletes all but the newest five `nightly.` releases and their
tags, so the releases page stays readable. It runs locally too, with `gh` signed in.

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

`just package` builds the runtime first, then base R with it (`just base-r`, which downloads
the R source release `tools/base-r/VERSION` names and checks its SHA-256), and then `ry` with
`RY_EMBED_RUNTIME` and `RY_EMBED_BASE` naming the two. `crates/repl/build.rs` embeds both
zstd-compressed, along with `libR.dll.a` on Windows. On first use `ry` extracts the runtime into
`ry/runtime/<hash>/` and base R into `ry/base/<hash>/` of the user's cache. `ry` is also the R front end
that `ry-rcmd` is in a development build: ry's R home runs it through a link named `ry-rcmd`
(on Windows, hard links named `R.exe`, `Rscript.exe` and so on), and `ry` acts as the front end
when started under such a name.

ry looks for its runtime in `RY_LIBR`, then beside the executable, then in `deps/` and
`../lib/ry/` next to it, and only then falls back to the built-in one, so an installer may still
place `ry` in `bin/` and the library in `lib/ry/`. Base R is looked up the same way: `RY_BASE_R`,
then `base-r/` beside the executable, one or two directories up, or in `../lib/ry/`, then the built-in
copy. Only `ry R` needs a GNU R installation.

The Linux archive is built on Ubuntu 22.04, so it needs glibc 2.35 or newer. On macOS, `libR.dylib`
has the install name `@rpath/libR.dylib` and exports the same names as the Linux library, which is
checked by cross-compiling from Linux (`development/design/r-runtime.md`, section 11). The smoke
test's `RY_SMOKE_PACKAGES` step, which loads a CRAN binary of jsonlite under `ry run`, is the first
run on a real Mac.

The Windows archive is built for `x86_64-pc-windows-gnullvm` with MSYS2's clang64 toolchain, so
`ry.exe` and `R.dll` link the UCRT, as Rtools-built packages do. Both have been run under Wine only
(`development/design/r-runtime.md`, section 11); the smoke test, which also loads a CRAN binary of
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
9. Append this release's rows to the progress dashboard, as the next section describes.
10. Deploy the docs site so the news and installation pages show the release: in the Actions tab, open
    **Docs**, select **Run workflow**, and enter the branch `main`.

## The progress dashboard

The progress dashboard (https://claude.ai/artifact/HXbZ3kVWhfrM47TJZ68aq7) charts ry across
releases: speed and peak memory against GNU R, the cold check, compatibility, the mandate checklist
and the size of the code base. Its data lives in the dashboard's own store as one JSON file of rows
per dataset, and a release appends rows to those files. The page itself does not change.

`tools/bench/progress.py` measures a release. Each suite runs GNU R (`Rscript` on `PATH`) in the
same sitting as the ry binaries it is given, and the dashboard divides each ry figure by the GNU R
figure of its own sitting, so rows from different machines or days never mix. Each command prints
a JSON object that maps a dataset id to its new rows:

```bash
ry=release/X.Y.Z-beta/ry   # the unpacked Linux archive
export R_HOME=/usr/lib/R   # needed only by a build without an embedded base R
tools/bench/progress.py programs --release X.Y.Z-beta=$ry > programs.json
tools/bench/progress.py startup  --release X.Y.Z-beta=$ry > startup.json
tools/bench/progress.py memory   --release X.Y.Z-beta=$ry > memory.json
tools/bench/progress.py s4       --release X.Y.Z-beta=$ry > s4.json
tools/bench/progress.py check    --release X.Y.Z-beta=$ry --project benches/finbench > check.json
tools/bench/progress.py code     --rev X.Y.Z-beta > code.json
```

`check` needs the finance code base, from `tools/bench/generate.py --out benches --domain finance
--lines 300000 --files 500 --ry $ry`. Pass several `--release` flags to measure more than one build
in the sitting; a build of the checked-out commit is `--release main=target/release/ry`, which is
recorded as `main@<commit>`. Run on an otherwise idle machine, because the timings are wall times.

| Dataset | Rows come from |
| --- | --- |
| `perf_programs`, `perf_startup`, `perf_memory`, `perf_s4`, `perf_check` | `progress.py programs`, `startup`, `memory`, `s4` and `check` |
| `codebase_loc`, `codebase_tests` | `progress.py code --rev TAG` |
| `compat_records` | one row per changed figure (oracle cases, corpus package tests, installs, suite tests) from `oracle compare --engine ry`, `oracle packages` and `oracle bench`, with `origin: "recorded"` and a link to where the figure is recorded |
| `perf_suites`, `check_records` | the same, for suite times against GNU R and the cold check of the three benches |
| `features`, `deps`, `cran_popular` | edited rows: a feature's status and evidence, a dependency's install and test result, a CRAN package's test result |

To append, an agent with the Artifact tools reads each dataset's document `datasets/<id>` from the
dashboard's store, downloads the file its `source.url` names, runs
`tools/bench/progress.py merge programs.json DIR` (which skips rows already present), uploads the
merged file as an asset, and updates the document's `source` and `updated`. Never edit or drop a
measured row; a new sitting adds rows, and the dashboard shows each release's newest sitting.
