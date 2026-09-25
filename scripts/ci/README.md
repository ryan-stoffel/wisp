# CI entry points

Workflows call these scripts instead of running cargo or npm themselves, so a local run is the same as a CI run. Background: [0001](../../docs/decisions/0001-ci-before-product-code.md), which built CI against a stand-in Electron app until #38 switched it to the real `Wisp.app`.

Each script finds the repo root on its own, so it runs from any directory.

| Script | What it does | Called by |
| --- | --- | --- |
| `check-rust` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, and `cargo test` on the workspace, with `--locked`. The tests include the [protocol type](#protocol-types) checks. | `ci.yml` (#3) |
| `../editor/test-wisp` | Runs the tests of wisp's editor code in `editor/overlay/src/vs/platform/wisp` with upstream's Node test runner, after `prepare`, `npm ci --ignore-scripts`, and upstream's `transpile-client`. They include an integration test against the `wispd` that `check-rust` built, in temporary data folders. It needs macOS, because `wispd` does, so it runs in the `rust` job, not the `fork` job. | `ci.yml` (#63), `rust` job |
| `check-fork` | Runs `scripts/editor/test`, then checks that the Code - OSS pin and patches apply (`scripts/editor/prepare`, then `scripts/editor/export-patches --check`) and that the root `.nvmrc` equals upstream's. It fails if an id in wisp's exclusion list (`editor/overlay/src/vs/workbench/common/wisp/exclusions.ts`) no longer appears in upstream's source. Then it type-checks `src/` in the patched tree with upstream's `npm run typecheck-client`, after `npm ci --ignore-scripts` when `node_modules` is missing and upstream's `node build/npm/electronTypes.ts`, which downloads the checksum-verified `electron.d.ts` | `ci.yml` (#8), `fork` job |
| `build-app` | Builds the packaged `Wisp.app` with `scripts/editor/build-app`, for this Mac's architecture or the one named (`arm64` or `x64`), and prints its path. It is the app `app-launch` launches by default. | `screenshots.yml` (#4), `capture` job, when the app cache misses |
| `check-app` | Checks a packaged `Wisp.app`, or a zip of one, for an architecture: its signature, its main binary's architecture, its `product.json` against `editor/product.json`, its bundle id, icon, `wisp` launcher and `wisp --version`, and `wispd` and `wispd --version` (#62) | `ci.yml` (#9), `app` and `app-x64` jobs |
| `app-cache-key` | Prints the cache key of the packaged app for an architecture (see [The app job](#the-app-job)) | `ci.yml` (#9), `app` and `app-x64` jobs (#177); `screenshots.yml`, `capture` job |
| `app-launch` | Prints Playwright `_electron.launch` options for a packaged `Wisp.app` as one line of JSON (see [app-launch](#app-launch)) | `screenshots` |
| `screenshots` | Captures every scenario in `ci/screenshots/` from the app into a directory (see [Screenshots](#screenshots)) | `screenshots.yml` (#4), `capture` job |
| `publish-screenshots` | Checks a capture directory, commits its PNGs to the `ci-screenshots` branch, and creates or updates the PR comment. It needs Actions' environment; locally, `--dry-run` prints the comment | `screenshots.yml` (#4), `publish` job |
| `check-screenshots` | `npm ci`, then lint, type-check, and test `ci/screenshots/`, including the tests that guard the `publish` job | `ci.yml` (#39), `screenshots` job |
| `smoke` | Runs the Playwright smoke checks in `ci/smoke/` against the app (see [Smoke tests](#smoke-tests)) | `ci.yml` (#13), `smoke` job |
| `check-smoke` | `npm ci`, then lint, type-check, and test `ci/smoke/` | `ci.yml` (#13), `smoke` job |
| `package-app` | Builds `Wisp.app` from source with a version stamped in, zips it, checks the zip with `check-app`, and prints the bundle path (see [package-app](#package-app)) | `release.yml` (#5), `build` job |
| `next-version` | Prints the version the next release gets, from tags and Conventional Commits (see [Releases](#releases)) | `release.yml` (#5), `build` job |
| `generate-cask` | Prints the Homebrew cask for a version and its zips, from `release/wisp.rb.template` | `release.yml` (#5), `build` job |
| `audit-cask` | Runs `brew style` and `brew audit` on a cask in a throwaway tap, then installs it, runs the `wisp` and `wispd` commands it links, and uninstalls it | `release.yml` (#5), `build` job |
| `check-release-artifact` | Checks that a downloaded release artifact holds exactly the expected zips and a `wisp.rb` that matches them | `release.yml` (#5), `release` job |
| `publish-cask` | Commits the cask to `ryan-stoffel/homebrew-taps` with `TAP_GITHUB_TOKEN`, and refuses to replace a newer version; `--check` only tests the token | `release.yml` (#5), `release` job |
| `check-release` | Unit tests for the release scripts | `release.yml` (#5), `build` job |

## Requirements

- Rust: rustup. `rust-toolchain.toml` pins the toolchain, its components, and both Darwin targets (`aarch64-apple-darwin` and `x86_64-apple-darwin`, for `build-app` to cross-compile `wispd`). In CI, run `rustup toolchain install` with no arguments as its own step before `check-rust` or `build-app`. It installs exactly what the file pins, targets included, and it does not rely on rustup's auto-install, which can be turned off. Locally, rustup installs the pin, and any target it is missing, on first use.
- Node: the exact version in the root `.nvmrc`. It always equals upstream's `.nvmrc` at the pinned Code - OSS release: `scripts/editor/upgrade` copies it, and `check-fork` fails if the two differ. In Actions, use `actions/setup-node` with `node-version-file: .nvmrc`. The scripts that run Node stop with an error when `node` has a different major version, so local runs use the same Node as CI. The `engines` field in `ci/screenshots/package.json` gives only the oldest Node 24 that the package supports; `.nvmrc` decides what runs.
- macOS, for `build-app`, `app-launch`, `screenshots`, and `package-app`.
- mikefarah's `yq` 4, for `app-cache-key`. GitHub's runners have it.
- Homebrew, for `audit-cask`. GitHub's macOS runners have it.

## Protocol types

`crates/wisp-protocol` is the source of the editor's protocol types ([0007](../../docs/decisions/0007-editor-wispd-protocol.md)). Two of its tests run in `check-rust`:

- **Stale TypeScript.** The generated file, `editor/overlay/src/vs/platform/wisp/common/wispProtocol.ts`, must match the Rust types. When it doesn't, the test names the command that regenerates it: `cargo run -p wisp-protocol --bin generate-typescript`. Commit the result. The file is under `editor/`, so a protocol change also reruns the `fork` job.
- **Samples.** Every message in `crates/wisp-protocol/samples/v<N>/` must still decode, so a change that is not additive fails. The rules for adding samples are in the crate's docs.

## app-launch

The output is one line with absolute paths:

```json
{"executablePath":"<repo>/editor/VSCode-darwin-arm64/Wisp.app/Contents/MacOS/Wisp","args":[]}
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

- It launches a packaged `Wisp.app`: `WISP_APP_BUNDLE` when that is set, otherwise the app that `build-app` built for this Mac's architecture, `editor/VSCode-darwin-<arch>/Wisp.app`. The executable is the bundle's `CFBundleExecutable`, with no arguments.
- If there is no app, `app-launch` explains why on stderr, prints nothing to stdout, and exits 1.
- Every key in the output is an `_electron.launch` option. It prints no `env` today. Merge it anyway, as above, so that an addition cannot drop `PATH`, `HOME`, and the rest.
- Playwright adds its startup hook only when it locates Electron itself. With `executablePath`, the app starts without waiting for Playwright, so wait for `firstWindow()` before inspecting windows with `electronApp.evaluate()`.
- The development build (`./scripts/code.sh`, see [editor-upgrade.md](../../docs/editor-upgrade.md)) is not an option, because the screenshots show the app that ships.

## Screenshots

`screenshots.yml` runs on every PR from a branch in this repo. It has two jobs, so the PR's build and its dependencies never run where the write token is; the publish script and this workflow itself still come from the PR head until #48:

- `capture` (macOS, `contents: read`) gets the arm64 `Wisp.app` (see [Getting the app](#getting-the-app)), runs `screenshots <dir>`, and uploads the PNGs, `manifest.json`, and both steps' logs as the `screenshots` artifact. `screenshots` installs `ci/screenshots/`, whose only runtime dependency is `playwright-core`, and runs each scenario in `ci/screenshots/src/scenarios.ts` against a fresh launch of the app from `app-launch`'s output. It exits 1 if any scenario failed. For PRs from forks, whose token is read-only, the job logs a notice and skips the rest, and `publish` does not run.
- `publish` (Linux, `contents: write` and `pull-requests: write`) runs even when `capture` failed. It checks out only `scripts/ci/` and `ci/screenshots/src/`, installs nothing, downloads the artifact, and runs `publish-screenshots`. That commits the PNGs to the orphan branch `ci-screenshots` under `pr-<number>/<short-sha>/`, then creates or updates the one comment by `github-actions[bot]` that contains `<!-- wisp-screenshots -->`. The images are `raw.githubusercontent.com` URLs pinned to the `ci-screenshots` commit, so no cache shows an old image. Nothing is deleted from `ci-screenshots` yet (#40).

Rules that keep the token away from the PR's build:

- `publish.ts` and the files it imports (`artifact.ts`, `branch.ts`, `comment.ts`, `manifest.ts`) use only Node built-ins. A test enforces this, and the `publish` job has no `node_modules`, so a package import fails instead of running.
- `publish` treats the artifact as untrusted. `manifest.json` must parse into the known shape. Each file must be `<name>.png` or `<name>.failed.png` for a listed scenario, a regular file, at most 10 MB, and start with the PNG signature; the files together must total at most 25 MB. Titles and reasons must be plain text. Only the last 64 KB of `build.log` and `capture.log` are read, and only when the job outputs say that step failed. If anything fails these checks, nothing is pushed, the comment says the results were rejected, and the `publish` job fails. If `capture` succeeded but its artifact never reaches `publish` (a lost upload or a failed download), `publish` reports the results as missing and fails instead of passing silently.
- No repository secret goes into the `capture` job. Its logs are written with `tee`, so Actions' masking does not apply, and their tails are posted in the comment. The job's one credential is its own `contents: read` `GITHUB_TOKEN`, which only the build step gets, and only on a cache miss. It reads nothing that is not public, and it expires when the job ends, before `publish` reads any log.

### Getting the app

`capture` shows the app that ships, not the development build:

1. It runs `app-cache-key arm64`, which gives the same key as `ci.yml`'s `app (arm64)` job.
2. It restores `dist/editor/wisp-darwin-arm64.zip` under that key with `actions/cache/restore`. On a hit, it unpacks the zip with `ditto -x -k` and sets `WISP_APP_BUNDLE`.
3. On a miss, the `build` step runs `build-app arm64` itself. That step gets the `GITHUB_TOKEN`, because upstream's build calls GitHub's API and anonymous calls from shared runners hit the rate limit.
4. The job summary says which of the two happened.

`capture` never saves to that cache. The `app` job stays its only writer, and saves only builds that passed `check-app`. Screenshots are informational, so a cached app is good enough here; releases never use one.

A push that changes no fork input hits: the zip comes from an earlier run of the same PR or from `develop`. A push that changes a fork input misses in both workflows at once, because `screenshots.yml` cannot wait on a job in `ci.yml`. On that push `capture` builds arm64 while `ci.yml`'s `app` job (and, on a push to `develop` or `main`, `app-x64`) builds it too. So the PR's first screenshots arrive about as fast as the `app` job's own cold run, within #9's 30 minutes, at the cost of one extra arm64 build, about 20 macOS runner minutes. On #89's first push, a miss, `capture` took 19 min 54 s: 18 min 16 s to build and 56 s for the three scenarios. The `app` job's legs took 21 min (arm64) and 23 min 33 s (x64), back when both built in one matrix job (#177 split them). On the next push, a hit, `capture` took 1 min 40 s: 7 s to restore the zip, 10 s to unpack it, and 55 s for the scenarios.

Waiting for the `app` job instead would not be faster, because the wait lasts as long as the build. It would also need a token that can read Actions, polling, and a way to notice a failed `app` job. And it could never finish when the keys differ: `ci.yml` builds the PR's merge commit, while `capture` checks out the head, whose screenshots the comment shows. When a PR is behind `develop` on a fork input, the `app` job saves a key that `capture` never asks for. A build handles that case, and an evicted cache entry too.

### Adding a view

Add an entry to `scenarios` in `ci/screenshots/src/scenarios.ts`. The workflow does not change.

- `name`: the file name, in lowercase words joined by hyphens.
- `title`: the heading and alt text in the comment. Plain text: letters, digits, spaces, and `, . : ; ' " ( ) / + & = _ -`.
- `settings` (optional): user settings the scenario starts with, written to its throwaway profile's `settings.json`, such as `wisp.host`.
- `args(dir)` (optional): returns extra app arguments, such as a folder and a file to open. `dir` is an empty directory for the scenario's own files, deleted afterwards. Copy fixtures into it rather than opening them in the checkout, so the app never writes into the repo. A test checks that every absolute path it returns is inside `dir`.
- `run({ app, window })`: drives the app and returns `screenshot(window)`. If this build does not have the view yet, it returns `notAvailable(reason)` instead: the comment lists the view as not available, and the job still passes. The reason follows the same plain-text rule as `title`, so it cannot hold `#123` or `@name`, which would notify that issue or person from every PR. If the view exists but breaks, `run` throws. The `capture` job then fails, and the comment shows the error and a screenshot of the window at that moment.

Before `run` is called:

- The first window's content area is 1024x640.
- The workbench has restored: `.monaco-workbench` exists, and upstream's `code/didStartWorkbench` performance mark is set.
- The DOM has been quiet for 500 ms, or 10 s have passed.

After `app-launch`'s `args`, the harness passes these arguments, then the scenario's own:

- `--user-data-dir` and `--extensions-dir` in a new temp directory.
- `--skip-welcome`, `--skip-release-notes`, `--disable-workspace-trust`, and `--use-inmemory-secretstorage`, the flags upstream's smoke tests use. A run then never reads the machine's extensions or keychain.
- `WISPD_DATA_DIR` in the same temp directory, so the wispd a scenario starts never touches the machine's own data folder. The harness stops that wispd when the scenario ends, using the pid in its `wispd.lock`.
- Not `--enable-smoke-test-driver`, because it hides notification toasts, and the screenshots should show what a user sees.

Launching gets 60 seconds for the process and 60 for the first window, then waiting for the window plus `run` gets 180 seconds. The `capture` step has 20 minutes, and when a step times out, `publish` still reports it.

The shipped scenarios wait for these elements, using the classes and attributes that upstream's smoke tests use, never pixel positions:

- `agents-window` launches with no arguments, which opens the Agents window (0011), and the app's bundled wispd starts through `wispd attach` as it does for a user. It waits for the title bar and wisp's sidebar view (`.part.titlebar`, `.part.sidebar .wisp-threads`), then for the sidebar's host chip to be connected (`button.wisp-threads-host[data-kind="connected"]`, labeled "Host: this Mac, connected") and the no-host view to be gone.
- `agents-window-disconnected` starts with `wisp.host` set to `ssh://127.0.0.1:9`, a port nothing listens on, so ssh fails at once with "connection refused". (An unresolvable name can wait 30 s or more on DNS, past the handshake timeout.) It waits for the chip's error state and fails unless the no-host view says "Can't reach ssh://127.0.0.1:9.", its composer reads "Reconnect to send messages", and the composer and Send are `aria-disabled`.
- `agents-window-host-menu` waits for the connected state, clicks the host chip, and waits for the host menu's Quick Pick with the current host and Reconnect.
- `startup` opens an empty editor window with `--new-window` and waits for the title bar, activity bar, editor, and status bar parts.
- `editor-file-open` copies `ci/screenshots/fixtures/workspace/` into its directory and opens that folder with `src/tasks.ts`. It waits for three things:
  - the editor, `.monaco-editor[data-uri$="/src/tasks.ts"]`
  - the active tab, `.tab.active[data-resource-name="tasks.ts"]`
  - the file's row in the explorer

### Running it locally

```sh
scripts/ci/build-app            # or set WISP_APP_BUNDLE to a Wisp.app you already have
scripts/ci/screenshots
scripts/ci/publish-screenshots --dry-run
scripts/ci/check-screenshots
```

`screenshots` writes to `ci/screenshots/out/`, and `publish-screenshots --dry-run` checks that directory the way the `publish` job does and prints the comment it would post. Add `--logs <dir>` with `BUILD_OUTCOME=failure` or `CAPTURE_OUTCOME=failure` to include a failed step's `build.log` or `capture.log`.

- A local run takes about 30 seconds once the app is built. `build-app` takes about 8 minutes on an M3 Pro from a clean tree, or 5 once `node_modules` is installed.
- The windows open on your screen. A Retina display doubles the image size.
- The app follows macOS's Increase Contrast setting, as any build of the editor does, so with it on you get the high contrast theme.

## Smoke tests

`ci.yml`'s `smoke` job (macOS, `needs: [app]`) is M0's automated check that the fork still builds with the editor, file tree, and terminal (#13). It restores the arm64 zip the `app` job above just built or restored ([Getting the app](#getting-the-app) describes the same restore; `smoke` runs after `app` in the same workflow instead of alongside it in a separate one, so the cache is always a hit here and the job adds only its own runtime, never a second build). `ci/smoke/` is a sibling package to `ci/screenshots/`; its `src/harness.ts` imports `ci/screenshots/src/harness.ts`'s launch code by relative path instead of duplicating it, which is why both packages get `npm ci` in `scripts/ci/smoke` and `scripts/ci/check-smoke` (`ci/screenshots/src/harness.ts`'s own `playwright-core` import resolves from `ci/screenshots/node_modules`, not `ci/smoke/node_modules`, since Node resolves a bare specifier from the importing file's own location).

Every launch gets its own throwaway `HOME` on top of the throwaway `--user-data-dir` and `--extensions-dir` `ci/screenshots/src/harness.ts` already makes, because the terminal check spawns the user's real shell (which reads rc files from `HOME`) and the Source Control check reads git's user config from it; neither should depend on, or be slowed down by, whatever is on the machine running the job.

The suite has three parts, in `ci/smoke/src/checks/`:

- `agents-window.ts`: a plain launch opens the Agents window (`sessions.html`) with wisp's sidebar and the disabled no-host composer, no dialog, and no sign-in, account, or Copilot text in the title bar.
- `editor.ts`: `wisp <folder>` opens the editor window (`workbench.html`) and, in one launch, exercises the file tree, editing and saving, search, Source Control, and a terminal, then checks the window for Chat UI and the command palette for chat, Copilot, or sign-in entries. One launch instead of six keeps the job's runtime down.
- `exclusions.ts`: #94's ask that the suite check, at runtime, that every id wisp excludes (`editor/overlay/src/vs/workbench/common/wisp/exclusions.ts` and `.../platform/wisp/common/excludedActions.ts`) is actually skipped. `ci/smoke/src/exclusions.ts` parses those files directly (accepting either quote style, a missing trailing comma, and a trailing comment, unlike `check-fork`'s sed, which #94 found only matches one shape), so a renamed or mis-quoted id shows up here even when it would silently vanish from `check-fork`'s guard. A table in the test gives every parsed id one of four kinds of evidence: a DOM element that would exist under its own id, a command palette entry with recognizable text, `app.getAppMetrics()` showing no local agent host utility process, or, for ids with no independent effect from outside the process (about half of them: an internal sync, a context key with no menu entry, or a feature reachable only through another excluded surface that is itself checked), `check-fork`'s static guard as the only automated check today. The table's count is itself asserted against the parsed lists, so a new exclusion has to be triaged into it or the suite fails. #144 tracks giving the static-only ids a real runtime signal.

### The known flake

#10's Handoff comment: "The first Playwright run against a newly built packaged app lost its window partway through, and I could not reproduce it in three more runs." `ci/screenshots/src/harness.ts`'s `launch()` used to trust `app.firstWindow()`, whichever `BrowserWindow` Electron creates first, at whatever URL it has at that instant (usually still `about:blank`). On a cold launch that first window can be a transient page that closes again before the caller gets to it, losing the window Playwright was watching. `launch()` now waits for a window whose navigation actually reaches `workbench.html` or `sessions.html`; a transient window's wait for that URL simply times out, and Playwright keeps waiting for the real one. One wait, no retry, and both `ci/screenshots` and `ci/smoke` get the fix.

### Running it locally

```sh
scripts/ci/build-app            # or set WISP_APP_BUNDLE to a Wisp.app you already have
scripts/ci/smoke
scripts/ci/check-smoke
```

Each launch opens on your screen, like `scripts/ci/screenshots`'s do.

## The app job

`ci.yml` builds the packaged `Wisp.app` in two jobs, one per architecture, so a PR's required `ci` check never waits on x64 (#177):

- `app` builds arm64. It runs on every PR and every push.
- `app-x64` builds x64. It has `if: github.event_name == 'push'`, so it only runs on a push to `develop` or `main`, never on a `pull_request`. Releases ship arm64 only (#43), and the `screenshots` and `smoke` jobs use the arm64 build, so nothing else in `ci.yml` needs x64 for a PR to pass. `ci`'s aggregate step treats `app-x64`'s normal PR outcome, `skipped`, as success, but still fails on an actual `failure`, which can only happen on a push.

Each job rebuilds only when its own inputs changed since a run that saved a build:

1. `app-cache-key <arch>` hashes its inputs: `git ls-files -s` over `editor/`, `.nvmrc`, `.gitattributes`, `scripts/editor/`, `Cargo.lock`, `crates/`, `daemon/`, and `rust-toolchain.toml`, plus the matching job's own definition (`yq '.jobs.app'` for arm64, `yq '.jobs.app-x64'` for x64). The cache key is `wisp-app-darwin-<arch>-<hash>`. Edits to other jobs, the docs, or the rest of `scripts/ci/` do not rebuild the app.
2. It restores `dist/editor/wisp-darwin-<arch>.zip` from the cache under that key.
3. On a miss, it runs `scripts/editor/build-app <arch>` and zips the bundle with `ditto`, the way `package-app` does. x64 cross-builds on the arm64 runner.
4. On a hit or a build, it runs `check-app` on the zip.
5. After a build, it saves the zip under the key.

A rebuild takes about 20 minutes per architecture on `macos-26`: `npm ci` 5 min, gulp 9 min, `cargo build --release -p wispd` under a minute warm, signing and the Mach-O check 3 min, and the zip and checks 1 to 2 min. On a PR, only arm64 rebuilds. On a push to `develop` or `main` where both `app` and `app-x64` rebuild, the two jobs run in parallel, so a push pays about the same wall-clock time as a PR's arm64-only build, at the cost of an extra 20 macOS runner-minutes. On #73, before this split, the whole `ci.yml` run took 21 to 24 minutes when both architectures rebuilt in one matrix job. A cache hit takes under 2 minutes: about 10 s to restore and 20 s to check arm64. x64 takes 80 s to check, because `wisp --version` runs under Rosetta. The whole run then takes about 2 minutes. Each build stores two zips, about 670 MB. When the repository's caches pass 10 GB, GitHub evicts the least recently used entries, and a PR whose build was evicted pays one rebuild. A plain macOS job per architecture beats compiling on `ubuntu-24.04` and packaging on macOS. Ubuntu compiles in 4 minutes instead of 8, but the macOS job still needs its own 5-minute `npm ci` for the native modules, and it cannot start until the Ubuntu job has uploaded a 766 MB artifact. #9's Progress comment has the numbers.

The key includes `wispd`'s own inputs (#62) so that a daemon-only change, with no `editor/` change, still rebuilds the cached app instead of shipping a stale `wispd`. The alternative, bundling `wispd` into the cached zip after restoring it, would need a second signing pass in the workflow, because the copy has to happen before the app is signed for the signature to cover it (see [package-app](#package-app)); that duplicates what `build-app` already does locally and in `screenshots.yml`. `wispd`'s own build is small next to gulp's 9 minutes, so a daemon-only PR pays about the same rebuild it already would for an editor change, still inside the 30-minute budget (#9), while an editor-only PR keeps hitting the cache exactly as before, since the Rust part of the key hash is unchanged.

Caches follow the same scopes as the `fork` job's markers. A PR can restore a build that `develop` saved, or one from an earlier push to the same PR. `develop` builds its own copy the first time its inputs change, because a PR's cache is not visible there. The key leaves out the runner image, as the `fork` job's does, so an image update does not rebuild the app until an input changes.

A build needs `GITHUB_TOKEN`: upstream's install and build download from GitHub (ripgrep, the built-in extensions, Electron), and anonymous API calls from shared runners hit the rate limit. The job gives it the workflow's `contents: read` token.

To use the cached app for checks elsewhere, do what `screenshots.yml` does ([Getting the app](#getting-the-app)): run `app-cache-key`, restore the zip with `actions/cache/restore`, unzip it with `ditto -x -k`, and never save to this cache. The key is one script so that the two workflows cannot drift apart. On a PR that changes an input, the key misses until this job has saved the new build.

Never ship the cached app. `release.yml` builds the release app from source and never restores this cache. A cache entry has no provenance: any job in a `develop` run can write one under this predictable key, npm install scripts and cargo build scripts included. `check-app` checks the branding, not where the zip came from.

## package-app

`scripts/ci/package-app <version> [arm64|x64]` packages for this Mac's architecture unless you name one. It prints the bundle's absolute path as the only line on stdout, for `WISP_APP_BUNDLE`, and sends everything else to stderr. It always builds from source. Steps:

1. Run `scripts/editor/build-app --app-version <version> <arch>`, which builds `editor/VSCode-darwin-<arch>/Wisp.app`:
   - It runs `prepare`, and installs `node_modules` for the architecture unless it already matches.
   - Just before gulp, it sets `version` in `editor/vscode/package.json`, and gulp copies it into the app. It puts `package.json` back on exit, even after a failure or an interrupt. So the version is never committed, the next `prepare` finds a clean tree, and publishing never pushes to `main`.
   - It builds `wispd` for the matching Rust target with `cargo build --release --locked -p wispd`, passing the version as `WISP_VERSION`, and copies the binary to `Contents/Resources/app/bin/wispd` (#44, #62).
   - It ad-hoc signs the whole bundle, wispd included, and checks that every Mach-O file is built for the architecture. Renaming Electron's bundle breaks its signature, and Gatekeeper reports an app with a broken signature as damaged, with no Open Anyway. #7 replaces the ad-hoc signature with Developer ID signing and notarization.
2. Check the bundle id `io.github.ryan-stoffel.wisp` and the version in each place it went:
   - `CFBundleShortVersionString` and `CFBundleVersion` in `Info.plist`, which Finder and Homebrew read.
   - `version` in the app's `product.json`, which the About dialog and `wisp --version` show, and in its `package.json`.
   - `wispd --version`, when it can run here (native arch, or x64 under Rosetta): it must print `wispd <version>`.
3. Zip the bundle with `ditto` to `dist/wisp-<version>-<arch>.zip`, the name the cask's `url` expects, and run `check-app` on the zip: its signature, branding, `wisp --version`, and `wispd --version`.

On an M3 Pro, `package-app 0.1.0 arm64` took 5 min 51 s with `node_modules` already installed, before `wispd` was part of the build; its own release build adds well under a minute once its dependencies are warm. Without `--app-version`, `build-app` builds the editor with upstream's version (1.139.0) and wispd with its own `Cargo.toml` placeholder (0.1.0).

`wispd`'s version comes in as `WISP_VERSION`, which `daemon/src/lib.rs` reads at compile time with `option_env!`, rather than by stamping `Cargo.toml`'s `[workspace.package].version` the way `editor/vscode/package.json` is stamped above (#44 proposed the `Cargo.toml` approach; #62 took this one instead). Cargo tracks `option_env!` reads in a crate's dependency fingerprint, so changing only `WISP_VERSION` between builds still rebuilds and relinks `wispd`, confirmed locally by building the same commit with two different values and no `cargo clean` in between. This never touches `Cargo.toml` or `Cargo.lock`, so there is nothing to restore afterward and no risk to a `--locked` build, `check-rust` included.

## Releases

`release.yml` runs only for PRs whose head is `develop` and whose base is `main`. A hotfix PR into `main` publishes nothing; it ships with the next develop-into-main release. Background: [0006](../../docs/decisions/0006-release-versioning-and-packaging.md).

It has two jobs, split the way `screenshots.yml` is:

- **`build`** runs on `macos-26` with `contents: read` and no secrets. Only its build step gets the job's read-only `GITHUB_TOKEN`, for upstream's GitHub downloads. It restores nothing from Actions' cache, not even `~/.npm`, so the release is built from source every time ([The app job](#the-app-job) says why). It runs the steps below, writes the version, the zip's sha256, and the cask to the job summary, and uploads the zips and `wisp.rb` as an artifact:
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
| `brew audit --cask --online --only=min_os,artifact_case,rosetta`, per zip | The zip is copied into Homebrew's download cache first. That lets these audits check the real artifact without a network download, although the release asset does not exist yet. They cover `depends_on macos` against the bundle's `LSMinimumSystemVersion`, the case of the `app` and `binary` paths, and the arm64 binary. |
| `brew install --cask`, then `brew uninstall --cask` | Proves the cask installs `Wisp.app` at the right version, that Homebrew quarantines it, as users get it, and that it links `$(brew --prefix)/bin/wisp` and `$(brew --prefix)/bin/wispd` into the app. Then it removes the quarantine, as the README tells users to, and checks that `wisp --version` and `wispd --version` both print the version. After the uninstall, both links must be gone. |

Left out:

- `--new`: its signing check fails by design for an app without a Developer ID, and its notability checks are for homebrew/cask.
- A plain `--online`: the release URL returns 404 until the release exists.
- `--signing`: disabled in Homebrew 7.

`audit-cask` uses a throwaway tap (`wisp-ci/dry-run`), a throwaway download cache, and a throwaway `--appdir`, so it never touches `/Applications`, and it never zaps. It skips the install check when a `wisp` cask is already installed or `$(brew --prefix)/bin/wisp` already exists, so running it on a dev Mac leaves Homebrew as it was. In Actions, where nothing should be installed, any skip fails the job instead.

The cask's `depends_on macos` must match the app's `LSMinimumSystemVersion`, or the `min_os` audit fails. Electron sets that value: 12.0 in Electron 43, which Code - OSS 1.139.0 uses. No Mach-O file in the app needs more: `vtool -show-build` reports a `minos` of 11.0 or 12.0 for each. Upstream's `microsoft-authentication` extension carries a broker that needs macOS 15, but #10 leaves that extension out. When an upgrade raises Electron's minimum, change `depends_on macos` in `release/wisp.rb.template` and the minimum in the README's Install section together.

If a run fails after the release exists, re-run it, but only while no newer release has shipped. The re-run reuses the tag and the release's zips, and `publish-cask` skips the commit when the tap already has the same cask. Once the tap has a newer version, `publish-cask` refuses to replace it and exits 1, so re-running an old run cannot downgrade users. If a failed run left a draft release, delete the draft first; the `release` job says so.

## Notes for workflows

- Worth caching: `~/.rustup/toolchains`, keyed on `rust-toolchain.toml`, because the runner's preinstalled stable never matches the pin; `~/.cargo/registry`, `~/.cargo/git`, and `target/`; and `~/.npm`, keyed on `ci/screenshots/package-lock.json` in the `screenshots` job and on `editor/upstream.json` wherever the editor is installed: the `fork` job, and the `app` and `capture` jobs when they build.
- Installing the editor is dominated by `npm ci`: 2 min 20 s cold and 2 min with a warm `~/.npm` on an M3 Pro. Most of that is native module builds and install scripts, so a warm `~/.npm` saves little. #8's Progress comment has the full numbers.
- `npm run download-builtin-extensions` calls the GitHub REST API. Pass `GITHUB_TOKEN` so that it does not share the anonymous rate limit.
- Upstream's lockfiles exist only after `scripts/editor/prepare` has run, so a cache step that runs before it, such as `actions/setup-node` with `cache: npm`, cannot key on them. Key editor caches on the committed `editor/upstream.json` and `editor/patches/**`, or run `prepare` before the cache step.
- The `fork` job runs on Ubuntu because upstream's type-check needs about 6.4 GB, which swaps on the 7 GB macOS runner, and it does not depend on the platform. Its check step usually takes about a minute and times out after 10. Once, on #89, it hung without output until the job's 30-minute limit.
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
