# CI entry points

Workflows call these scripts instead of running cargo or npm themselves, so a local run is the same as a CI run. When the Code - OSS fork (#8) replaces the Electron fixture, these scripts change and the workflows do not. Background: [0001](../../docs/decisions/0001-ci-before-product-code.md).

Each script finds the repo root on its own, so it runs from any directory.

| Script | What it does | Called by |
| --- | --- | --- |
| `check-rust` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, and `cargo test` on the workspace, with `--locked`. The tests include the [protocol type](#protocol-types) checks. | `ci.yml` (#3) |
| `../editor/test-wisp` | Runs the tests of wisp's editor code in `editor/overlay/src/vs/platform/wisp` with upstream's Node test runner, after `prepare`, `npm ci --ignore-scripts`, and upstream's `transpile-client`. They include an integration test against the `wispd` that `check-rust` built, in temporary data folders. It needs macOS, because `wispd` does, so it runs in the `rust` job, not the `fork` job. | `ci.yml` (#63), `rust` job |
| `check-editor` | `npm ci`, then lint, type-check (`tsc --noEmit`), build, and test the editor, currently `ci/fixtures/electron-smoke/` | `ci.yml` (#3), `editor` job |
| `check-fork` | Runs `scripts/editor/test`, then checks that the Code - OSS pin and patches apply (`scripts/editor/prepare`, then `scripts/editor/export-patches --check`) and that the root `.nvmrc` equals upstream's. It fails if an id in wisp's exclusion list (`editor/overlay/src/vs/workbench/common/wisp/exclusions.ts`) no longer appears in upstream's source. Then it type-checks `src/` in the patched tree with upstream's `npm run typecheck-client`, after `npm ci --ignore-scripts` when `node_modules` is missing and upstream's `node build/npm/electronTypes.ts`, which downloads the checksum-verified `electron.d.ts` | `ci.yml` (#8), `fork` job |
| `build-app` | Installs, builds, and downloads the Electron binary: what `app-launch` needs, without lint or tests. `WISP_APP=editor` builds the Code - OSS development build in `editor/vscode/` instead of the fixture. The packaged `Wisp.app` comes from `scripts/editor/build-app` instead (see [The app job](#the-app-job)). | `screenshots.yml` (#4) |
| `check-app` | Checks a packaged `Wisp.app`, or a zip of one, for an architecture: its signature, its main binary's architecture, its `product.json` against `editor/product.json`, its bundle id, icon, and `wisp` launcher, and `wisp --version` | `ci.yml` (#9), `app` job |
| `app-launch` | Prints Playwright `_electron.launch` options for the built app as one line of JSON. `WISP_APP=editor` selects the Code - OSS development build. | `screenshots.yml` (#4) |
| `screenshots` | Captures every scenario in `ci/screenshots/` from the built app into a directory (see [Screenshots](#screenshots)) | `screenshots.yml` (#4), `capture` job |
| `publish-screenshots` | Checks a capture directory, commits its PNGs to the `ci-screenshots` branch, and creates or updates the PR comment. It needs Actions' environment; locally, `--dry-run` prints the comment | `screenshots.yml` (#4), `publish` job |
| `check-screenshots` | `npm ci`, then lint, type-check, and test `ci/screenshots/` | Not yet: #39 adds it to `ci.yml` |
| `package-app` | Builds `wisp.app` with a version stamped in, ad-hoc signs it, zips it, and prints the bundle path (see [package-app](#package-app)) | `release.yml` (#5), `build` job |
| `next-version` | Prints the version the next release gets, from tags and Conventional Commits (see [Releases](#releases)) | `release.yml` (#5), `build` job |
| `generate-cask` | Prints the Homebrew cask for a version and its zips, from `release/wisp.rb.template` | `release.yml` (#5), `build` job |
| `audit-cask` | Runs `brew style` and `brew audit` on a cask in a throwaway tap, then installs and uninstalls it | `release.yml` (#5), `build` job |
| `check-release-artifact` | Checks that a downloaded release artifact holds exactly the expected zips and a `wisp.rb` that matches them | `release.yml` (#5), `release` job |
| `publish-cask` | Commits the cask to `ryan-stoffel/homebrew-taps` with `TAP_GITHUB_TOKEN`, and refuses to replace a newer version; `--check` only tests the token | `release.yml` (#5), `release` job |
| `check-release` | Unit tests for the release scripts | `release.yml` (#5), `build` job |

## Requirements

- Rust: rustup. `rust-toolchain.toml` pins the toolchain and its components. In CI, run `rustup toolchain install` with no arguments as its own step before `check-rust`. It installs exactly what the file pins, and it does not rely on rustup's auto-install, which can be turned off. Locally, rustup installs the pin on first use.
- Node: the exact version in the root `.nvmrc`. It always equals upstream's `.nvmrc` at the pinned Code - OSS release: `scripts/editor/upgrade` copies it, and `check-fork` fails if the two differ. In Actions, use `actions/setup-node` with `node-version-file: .nvmrc`. The scripts that run Node stop with an error when `node` has a different major version, so local runs use the same Node as CI.
- macOS, for `build-app`, `app-launch`, and `package-app`.
- Homebrew, for `audit-cask`. GitHub's macOS runners have it.

## Protocol types

`crates/wisp-protocol` is the source of the editor's protocol types ([0007](../../docs/decisions/0007-editor-wispd-protocol.md)). Two of its tests run in `check-rust`:

- **Stale TypeScript.** The generated file, `editor/overlay/src/vs/platform/wisp/common/wispProtocol.ts`, must match the Rust types. When it doesn't, the test names the command that regenerates it: `cargo run -p wisp-protocol --bin generate-typescript`. Commit the result. The file is under `editor/`, so a protocol change also reruns the `fork` job.
- **Samples.** Every message in `crates/wisp-protocol/samples/v<N>/` must still decode, so a change that is not additive fails. The rules for adding samples are in the crate's docs.

## app-launch

The output is one line with absolute paths:

```json
{"executablePath":"<repo>/ci/fixtures/electron-smoke/node_modules/electron/dist/Electron.app/Contents/MacOS/Electron","args":["<repo>/ci/fixtures/electron-smoke/out/main.js"]}
```

Resolve the script from the repo root, so the caller's working directory does not matter. Merge `env` into your own environment, because `_electron.launch` uses `env` as the whole environment instead of adding to it:

```js
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { _electron } from '@playwright/test';

const repoRoot = execFileSync('git', ['rev-parse', '--show-toplevel'], { encoding: 'utf8' }).trim();
const options = JSON.parse(execFileSync(join(repoRoot, 'scripts/ci/app-launch'), { encoding: 'utf8' }));
const electronApp = await _electron.launch({ ...options, env: { ...process.env, ...options.env } });
const window = await electronApp.firstWindow();
```

- The fixture opens one window titled `wisp`.
- If the app is not built, `app-launch` explains why on stderr, prints nothing to stdout, and exits 1. Run `build-app` first, with the same `WISP_APP`.
- `WISP_APP_BUNDLE=/path/to/wisp.app` launches a built bundle instead: the bundle's `CFBundleExecutable`, with no arguments.
- Every key in the output is an `_electron.launch` option. The fixture prints no `env`. The editor prints only additions, and the merge above keeps `PATH`, `HOME`, and the rest.
- Playwright adds its startup hook only when it locates Electron itself. With `executablePath`, the app starts without waiting for Playwright, so wait for `firstWindow()` before inspecting windows with `electronApp.evaluate()`.

### The Code - OSS development build

`WISP_APP=editor` prints the development build in `editor/vscode/` ([0002](../../docs/decisions/0002-editor-fork-strategy.md)). Build it with `WISP_APP=editor scripts/ci/build-app`. The executable is the Electron app that upstream downloads into `.build/electron/`. The source tree is its first argument, and `env` holds the variables that upstream's `scripts/code.sh` sets:

```json
{"executablePath":"<repo>/editor/vscode/.build/electron/Wisp.app/Contents/MacOS/Wisp","args":["<repo>/editor/vscode","--disable-extension=vscode.vscode-api-tests"],"env":{"NODE_ENV":"development","VSCODE_DEV":"1","VSCODE_CLI":"1","ELECTRON_ENABLE_STACK_DUMPING":"1","ELECTRON_ENABLE_LOGGING":"1"}}
```

- Add arguments after the printed ones, and keep the source tree first, as the screenshot harness does. Add `--extensions-dir=<dir>` as well as a fresh `--user-data-dir` to keep a run apart from any other Code - OSS development build on the machine.

## Screenshots

`screenshots.yml` runs on every PR from a branch in this repo. It has two jobs, so the PR's build and its dependencies never run where the write token is; the publish script and this workflow itself still come from the PR head until #48:

- `capture` (macOS, `contents: read`) runs `build-app`, then `screenshots <dir>`, and uploads the PNGs, `manifest.json`, and both steps' logs as the `screenshots` artifact. `screenshots` installs `ci/screenshots/`, whose only runtime dependency is `playwright-core`, and runs each scenario in `ci/screenshots/src/scenarios.ts` against a fresh launch of the app from `app-launch`'s output. It exits 1 if any scenario failed. For PRs from forks, whose token is read-only, the job logs a notice and skips the rest, and `publish` does not run.
- `publish` (Linux, `contents: write` and `pull-requests: write`) runs even when `capture` failed. It checks out only `scripts/ci/` and `ci/screenshots/src/`, installs nothing, downloads the artifact, and runs `publish-screenshots`. That commits the PNGs to the orphan branch `ci-screenshots` under `pr-<number>/<short-sha>/`, then creates or updates the one comment by `github-actions[bot]` that contains `<!-- wisp-screenshots -->`. The images are `raw.githubusercontent.com` URLs pinned to the `ci-screenshots` commit, so no cache shows an old image. Nothing is deleted from `ci-screenshots` yet (#40).

Rules that keep the token away from the PR's build:

- `publish.ts` and the files it imports (`artifact.ts`, `branch.ts`, `comment.ts`, `manifest.ts`) use only Node built-ins. A test enforces this, and the `publish` job has no `node_modules`, so a package import fails instead of running.
- `publish` treats the artifact as untrusted. `manifest.json` must parse into the known shape. Each file must be `<name>.png` or `<name>.failed.png` for a listed scenario, a regular file, at most 10 MB, and start with the PNG signature; the files together must total at most 25 MB. Titles and reasons must be plain text. Only the last 64 KB of `build.log` and `capture.log` are read, and only when the job outputs say that step failed. If anything fails these checks, nothing is pushed, the comment says the results were rejected, and the `publish` job fails. If `capture` succeeded but its artifact never reaches `publish` (a lost upload or a failed download), `publish` reports the results as missing and fails instead of passing silently.
- Nothing secret goes into the `capture` job. Its logs are written with `tee`, so Actions' masking does not apply, and their tails are posted in the comment.

### Adding a view

Add an entry to `scenarios` in `ci/screenshots/src/scenarios.ts`. The workflow does not change.

- `name`: the file name, in lowercase words joined by hyphens.
- `title`: the heading and alt text in the comment. Plain text: letters, digits, spaces, and `, . : ; ' " ( ) / + & = _ -`.
- `args` (optional): extra app arguments, such as a folder and a file to open.
- `run({ app, window })`: drives the app and returns `screenshot(window)`. If this build does not have the view yet, it returns `notAvailable(reason)` instead: the comment lists the view as not available, and the job still passes. The reason follows the same plain-text rule as `title`, so it cannot hold `#123` or `@name`, which would notify that issue or person from every PR. If the view exists but breaks, `run` throws. The `capture` job then fails, and the comment shows the error and a screenshot of the window at that moment.

Before `run` is called, the first window has loaded, its content area is 1024x640, and for a Code - OSS window `.monaco-workbench` exists. After `app-launch`'s `args`, the harness passes `--user-data-dir=<new temp dir>`, `--skip-welcome`, `--skip-release-notes`, `--disable-workspace-trust`, and then the scenario's `args`. The fixture ignores all of them. Launching gets 60 seconds for the process and 60 for the first window, then waiting for the window plus `run` gets 180 seconds. The `capture` step has 20 minutes, and when a step times out, `publish` still reports it.

The shipped scenarios look for these hooks:

- `editor-file-open` opens `ci/screenshots/fixtures/workspace/` with `src/tasks.ts` and waits for `.monaco-editor[data-uri$="/src/tasks.ts"]`, the attribute upstream's smoke tests use.
- `coordinator-chat` waits up to 10 seconds for `.wisp-coordinator-chat`. #12 puts that class on the view's root element and, if the view is hidden at startup, adds the steps that reveal it.

### Running it locally

```sh
scripts/ci/build-app
scripts/ci/screenshots
scripts/ci/publish-screenshots --dry-run
scripts/ci/check-screenshots
```

`screenshots` writes to `ci/screenshots/out/`, and `publish-screenshots --dry-run` checks that directory the way the `publish` job does and prints the comment it would post. Add `--logs <dir>` with `BUILD_OUTCOME=failure` or `CAPTURE_OUTCOME=failure` to include a failed step's `build.log` or `capture.log`.

## The app job

`ci.yml`'s `app` job builds the packaged `Wisp.app` for arm64 and x64 on `macos-26`, one matrix leg each. A leg rebuilds only when its inputs changed since a run that saved a build:

1. It hashes its inputs: `git ls-files -s` over `editor/`, `.nvmrc`, `.gitattributes`, and `scripts/editor/`, plus the job's own definition (`yq '.jobs.app'`). The cache key is `wisp-app-darwin-<arch>-<hash>`. Edits to other jobs, the docs, or `scripts/ci/` do not rebuild the app.
2. It restores `dist/editor/wisp-darwin-<arch>.zip` from the cache under that key.
3. On a miss, it runs `scripts/editor/build-app <arch>` and zips the bundle with `ditto`, the way `package-app` does. x64 cross-builds on the arm64 runner.
4. On a hit or a build, it runs `check-app` on the zip.
5. After a build, it saves the zip under the key.

A rebuild takes about 20 minutes per architecture on `macos-26`: `npm ci` 5 min, gulp 9 min, signing and the Mach-O check 3 min, and the zip and checks 1 to 2 min. The two architectures build in parallel. On #73, the whole `ci.yml` run took 21 to 24 minutes when both rebuilt. A cache hit takes under 2 minutes: about 10 s to restore and 20 s to check arm64. x64 takes 80 s to check, because `wisp --version` runs under Rosetta. The whole run then takes about 2 minutes. Each build stores two zips, about 670 MB. When the repository's caches pass 10 GB, GitHub evicts the least recently used entries, and a PR whose build was evicted pays one rebuild. A plain macOS job per architecture beats compiling on `ubuntu-24.04` and packaging on macOS. Ubuntu compiles in 4 minutes instead of 8, but the macOS job still needs its own 5-minute `npm ci` for the native modules, and it cannot start until the Ubuntu job has uploaded a 766 MB artifact. #9's Progress comment has the numbers.

Caches follow the same scopes as the `fork` job's markers. A PR can restore a build that `develop` saved, or one from an earlier push to the same PR. `develop` builds its own copy the first time its inputs change, because a PR's cache is not visible there. The key leaves out the runner image, as the `fork` job's does, so an image update does not rebuild the app until an input changes.

A build needs `GITHUB_TOKEN`: upstream's install and build download from GitHub (ripgrep, the built-in extensions, Electron), and anonymous API calls from shared runners hit the rate limit. The job gives it the workflow's `contents: read` token.

To use the cached app for checks elsewhere, such as screenshots (#38), compute the same key, restore the zip with `actions/cache/restore`, and unzip it with `ditto -x -k`. On a PR that changes an input, the key misses until this job has saved the new build.

Never ship the cached app. `release.yml` builds the release app from source and never restores this cache. A cache entry has no provenance: any job in a `develop` run can write one under this predictable key, npm install scripts and cargo build scripts included. `check-app` checks the branding, not where the zip came from.

## Switching to the real app

#8 imported the fork:

- It added the development build to `build-app` and `app-launch` behind `WISP_APP=editor`, with the fixture still the default.
- It added `check-fork` and the `fork` job.
- It set the root `.nvmrc` to upstream's.

#9 added the packaged `Wisp.app` and its cached build, the `app` job. The fixture stays the default until #38:

- Makes the editor the default in `build-app` and `app-launch`, or sets `WISP_APP_BUNDLE` to the packaged `wisp.app`.
- Points `check-editor` at the fork's commands. #43 does the same for `package-app`.
- Deletes the fixture, as 0001 describes.

## package-app

`scripts/ci/package-app <version> [arm64|x64]` packages for this Mac's architecture unless you name one. It prints the bundle's absolute path as the only line on stdout, for `WISP_APP_BUNDLE`, and sends everything else to stderr. Steps:

1. Stamp `<version>` into the fixture with `npm version <version> --no-git-tag-version --allow-same-version`, which updates `package.json` and `package-lock.json` together, and set `productName` to `wisp`, so the app keeps its data in `~/Library/Application Support/wisp` ([0003](../../docs/decisions/0003-naming.md)). Both files are restored on exit, so the version is never committed and publishing never pushes to `main`.
2. Run `build-app`, then `@electron/packager`, into `dist/wisp-darwin-<arch>/wisp.app` with the bundle id `io.github.ryan-stoffel.wisp`.
3. Ad-hoc sign the whole bundle and check its signature, version, and bundle id. Packager keeps Electron's per-binary signatures, which no longer match the renamed bundle. Gatekeeper then reports the app as damaged and offers no Open Anyway. #7 replaces this step with Developer ID signing and notarization.
4. Zip the bundle with `ditto` to `dist/wisp-<version>-<arch>.zip`, the name the cask's `url` expects.

When #43 points it at the fork, only steps 1 and 2 change: stamp the version into `editor/vscode` and run `scripts/editor/build-app` instead of `@electron/packager`. Once `wispd` ships inside the bundle, stamp it too: after setting `version` in `[workspace.package]` in `Cargo.toml`, run `cargo update --workspace --offline`. Otherwise `Cargo.lock` keeps the old version, and every `--locked` build, `check-rust` included, fails with "cannot update the lock file".

## Releases

`release.yml` runs only for PRs whose head is `develop` and whose base is `main`. A hotfix PR into `main` publishes nothing; it ships with the next develop-into-main release. Background: [0006](../../docs/decisions/0006-release-versioning-and-packaging.md).

It has two jobs, split the way `screenshots.yml` is:

- **`build`** runs on `macos-26` with `contents: read` and no secrets. It runs the steps below, writes the version, the zip's sha256, and the cask to the job summary, and uploads the zips and `wisp.rb` as an artifact:
  - While the PR is open, that is the whole dry run, and the artifact is `wisp-<version>-dry-run`. Nothing is published.
  - When Ryan merges the PR, `build` runs on the merge commit and uploads `wisp-<version>`. First, it stops unless the PR was merged with a merge commit, whose second parent is the PR's head. A squash merge would make the version count the wrong commits. It also refuses a `v<version>` tag that sits on another commit. If `v<version>` is already released, it reuses the published zips instead of building new ones.
- **`release`** runs on `ubuntu-24.04` and only after a merge. It is the only job with `contents: write` and `TAP_GITHUB_TOKEN`, and it installs and builds nothing. It checks out only `scripts/ci/`. The scripts it runs use only Node built-ins, which `release/builtins.test.js` enforces. Before anything is tagged, it does the following:
  1. Stops if the secret is missing, or if `publish-cask --check` cannot read the tap with it (#28).
  2. Checks the artifact with `check-release-artifact`: exactly the zips for `RELEASE_ARCHES` plus a `wisp.rb` that matches the cask generated from them.
  3. Stops if an earlier run left a draft release. If `v<version>` is already published, it checks that the artifact's zips match the ones attached to it.

  Then it runs `gh release create v<version> --target <merge commit> --generate-notes` with the zips, which also creates the tag, and runs `publish-cask` to commit `Casks/wisp.rb` to the tap's default branch. If that push fails, the job prints what to fix and says to use **Re-run failed jobs**, which keeps the `build` artifact.

Merged runs never cancel one another, so two merges cannot compute the same version. A concurrency group holds only one waiting run, though. If a third merge arrives while one run is in progress and another is waiting, the waiting run is cancelled, and its changes ship in the third run's release.

The same steps run locally:

```sh
scripts/ci/check-release
version=$(scripts/ci/next-version)
scripts/ci/package-app "$version"
scripts/ci/generate-cask "$version" dist/wisp-"$version"-*.zip > dist/wisp.rb
scripts/ci/audit-cask dist/wisp.rb dist/wisp-"$version"-*.zip
```

`next-version` rules:

- With no `vX.Y.Z` tag merged into HEAD, the version is `0.1.0`.
- Otherwise, the commits since the last tag decide the bump:
  - A breaking change (`type!:` or a `BREAKING CHANGE:` footer) is a major bump from 1.0 on, and a minor bump before 1.0.
  - `feat` is a minor bump.
  - Anything else is a patch bump.
- A tag already on HEAD is reused, so re-running a release is safe.
- A shallow clone is refused, because older tags may be missing from it.

`audit-cask` runs these checks on Homebrew 7.0.6:

| Check | Why |
| --- | --- |
| `brew style --cask` | `brew audit` does not run RuboCop on casks, so the `desc` rules, stanza order, and `depends_on` style are only checked here. |
| `brew audit --cask --strict --arch=all` | Every offline audit, including the strict-only ones, for both architectures. |
| `brew audit --cask --online --only=min_os,artifact_case,rosetta`, per zip | The zip is copied into Homebrew's download cache first. That lets these audits check the real artifact without a network download, although the release asset does not exist yet. They cover `depends_on macos` against the bundle's `LSMinimumSystemVersion`, the case of the `app` name, and the arm64 binary. |
| `brew install --cask`, then `brew uninstall --cask` | Proves the cask installs `wisp.app` at the right version and that Homebrew quarantines it, as users get it. |

Left out:

- `--new`: its signing check fails by design for an app without a Developer ID, and its notability checks are for homebrew/cask.
- A plain `--online`: the release URL returns 404 until the release exists.
- `--signing`: disabled in Homebrew 7.

`audit-cask` uses a throwaway tap (`wisp-ci/dry-run`) and a throwaway download cache, and it never zaps. It skips the install check when a `wisp` cask is already installed, so running it on a dev Mac leaves Homebrew as it was.

If a run fails after the release exists, re-run it, but only while no newer release has shipped. The re-run reuses the tag and the release's zips, and `publish-cask` skips the commit when the tap already has the same cask. Once the tap has a newer version, `publish-cask` refuses to replace it and exits 1, so re-running an old run cannot downgrade users. If a failed run left a draft release, delete the draft first; the `release` job says so.

## Notes for workflows

- `check-editor` never downloads the Electron binary. Electron 44 fetches it on first use rather than at install, and `build-app` fetches it explicitly.
- Worth caching: `~/.rustup/toolchains`, keyed on `rust-toolchain.toml`, because the runner's preinstalled stable never matches the pin; `~/.cargo/registry`, `~/.cargo/git`, and `target/`; `~/.npm`, keyed on `ci/fixtures/electron-smoke/package-lock.json` in the `editor` job and on `editor/upstream.json` in the `fork` job; and `~/Library/Caches/electron` for the Electron download.
- Building the editor (`WISP_APP=editor`) is dominated by `npm ci`: 2 min 20 s cold and 2 min with a warm `~/.npm` on an M3 Pro. Most of that is native module builds and install scripts, so caching the installed `node_modules` directories saves more than caching `~/.npm`. #8's Progress comment has the full numbers for #9.
- `npm run download-builtin-extensions` calls the GitHub REST API. Pass `GITHUB_TOKEN` so that it does not share the anonymous rate limit.
- Upstream's lockfiles exist only after `scripts/editor/prepare` has run, so a cache step that runs before it, such as `actions/setup-node` with `cache: npm`, cannot key on them. Key editor caches on the committed `editor/upstream.json` and `editor/patches/**`, or run `prepare` before the cache step.
- The `fork` job runs on Ubuntu because upstream's type-check needs about 6.4 GB, which swaps on the 7 GB macOS runner, and it does not depend on the platform.
- A PR skips `check-fork` when its inputs already passed it:
  - The inputs are every tracked file under `editor/` (the pin, the patches, and anything #9 adds; `editor/vscode/` is never tracked), plus `.nvmrc`, `.gitattributes`, `scripts/editor/`, `check-fork`, and `ci.yml`.
  - The key is a SHA-256 of `git ls-files -s` over those paths, so each file's mode and path count as well as its content. With `hashFiles`, a `chmod -x` or a renamed patch would have kept an old pass.
  - Each pass saves an empty marker with `actions/cache` under that key. A PR can reuse a pass from `develop` or from an earlier push to the same PR.
  - Unlike a diff against the previous push, a later docs-only push cannot turn a failed check green.
  - The job always runs, so `ci` still gets its result. Pushes to `develop` and `main` always run the check.
- The skip is advisory, and the push run on `develop` is the check of record:
  - A PR's run executes the PR's own `ci.yml`. So a PR could save a marker under the key of inputs that fail, and skip the check.
  - That marker lands only in the PR's own cache scope, `refs/pull/<n>/merge`, which other PRs cannot read.
  - Only push runs write to the `develop` and `main` scopes. They execute the reviewed `ci.yml`, always run the full check, and save a marker only after a pass.
  - The worst case is a merged break that turns `develop` red on its next push. A PR could already cause that by editing `ci.yml`; a forged marker only hides the edit from the squashed diff.
  - A skip that cannot be forged would be decided from the merge commit instead (`fetch-depth: 2`, then `git diff --quiet HEAD^1 HEAD -- <inputs>`). It trusts only `develop`'s runs, but loses reuse within a PR.
