# CI entry points

Workflows call these scripts instead of running cargo or npm themselves, so a local run is the same as a CI run. When the Code - OSS fork (#8) replaces the Electron fixture, these scripts change and the workflows do not. Background: [0001](../../docs/decisions/0001-ci-before-product-code.md).

Each script finds the repo root on its own, so it runs from any directory.

| Script | What it does | Called by |
| --- | --- | --- |
| `check-rust` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, and `cargo test` on the workspace, with `--locked` | `ci.yml` (#3) |
| `check-editor` | `npm ci`, then lint, type-check (`tsc --noEmit`), build, and test the editor, currently `ci/fixtures/electron-smoke/` | `ci.yml` (#3) |
| `build-app` | Installs, builds, and downloads the Electron binary: what `app-launch` needs, without lint or tests | `screenshots.yml` (#4) |
| `app-launch` | Prints Playwright `_electron.launch` options for the built app as one line of JSON | `screenshots.yml` (#4) |
| `screenshots` | Captures every scenario in `ci/screenshots/` from the built app into a directory (see [Screenshots](#screenshots)) | `screenshots.yml` (#4) |
| `publish-screenshots` | Commits the captures to the `ci-screenshots` branch and creates or updates the PR comment. It needs Actions' environment; locally, `--dry-run` prints the comment | `screenshots.yml` (#4) |
| `check-screenshots` | `npm ci`, then lint, type-check, and test `ci/screenshots/` | Not yet: #39 adds it to `ci.yml` |
| `package-app` | Not yet written: #5 adds it (see [package-app](#package-app-added-by-5)) | `release.yml` (#5) |

## Requirements

- Rust: rustup. `rust-toolchain.toml` pins the toolchain and its components. In CI, run `rustup toolchain install` with no arguments as its own step before `check-rust`. It installs exactly what the file pins, and it does not rely on rustup's auto-install, which can be turned off. Locally, rustup installs the pin on first use.
- Node: the exact version in the root `.nvmrc`. In Actions, use `actions/setup-node` with `node-version-file: .nvmrc`. The scripts that run Node stop with an error when `node` has a different major version, so local runs use the same Node as CI.
- macOS, for `build-app` and `app-launch`.

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
- If the app is not built, `app-launch` explains why on stderr, prints nothing to stdout, and exits 1. Run `build-app` first.
- `WISP_APP_BUNDLE=/path/to/wisp.app` launches a built bundle instead: the bundle's `CFBundleExecutable`, with no arguments.
- Every key in the output is an `_electron.launch` option. The fixture prints no `env`. If a later app needs one, it lists only additions, and the merge above keeps `PATH`, `HOME`, and the rest.
- Playwright adds its startup hook only when it locates Electron itself. With `executablePath`, the app starts without waiting for Playwright, so wait for `firstWindow()` before inspecting windows with `electronApp.evaluate()`.

## Screenshots

On every PR from a branch in this repo, `screenshots.yml` runs `build-app`, then `screenshots <dir>`, then `publish-screenshots <dir>`. PRs from forks get a read-only token, so the job logs a notice and skips the rest.

- `screenshots` installs `ci/screenshots/`, whose only runtime dependency is `playwright-core`, and runs each scenario in `ci/screenshots/src/scenarios.ts` against a fresh launch of the app from `app-launch`'s output. It writes the PNGs and a `manifest.json` to `<dir>` (default `ci/screenshots/out/`) and exits 1 if any scenario failed.
- `publish-screenshots` commits the PNGs to the orphan branch `ci-screenshots` under `pr-<number>/<short-sha>/`, then creates or updates the one PR comment that contains `<!-- wisp-screenshots -->`. The images are `raw.githubusercontent.com` URLs pinned to the `ci-screenshots` commit, so no cache shows an old image. The step also runs after a failed build or capture, so the comment always says what happened. Nothing is deleted from `ci-screenshots` yet (#40).

### Adding a view

Add an entry to `scenarios` in `ci/screenshots/src/scenarios.ts`. The workflow does not change.

- `name`: the file name, in lowercase words joined by hyphens.
- `title`: the heading and alt text in the comment.
- `args` (optional): extra app arguments, such as a folder and a file to open.
- `run({ app, window })`: drives the app and returns `screenshot(window)`. If this build does not have the view yet, it returns `notAvailable(reason)` instead: the comment lists the view as not available, and the job still passes. If the view exists but breaks, `run` throws. The job then fails, and the comment shows the error and a screenshot of the window at that moment.

Keep `#123` and `@name` out of reasons. The comment is posted on every PR, and each mention would add a cross-reference to that issue.

Before `run` is called, the first window has loaded, and for a Code - OSS window `.monaco-workbench` exists. After `app-launch`'s `args`, the harness passes `--user-data-dir=<new temp dir>`, `--skip-welcome`, `--skip-release-notes`, `--disable-workspace-trust`, and then the scenario's `args`. The fixture ignores all of them.

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

`screenshots` writes to `ci/screenshots/out/`, and `publish-screenshots --dry-run` prints the comment it would post for that directory.

## Switching to the real app (#8)

When the fork builds, #8 does one of these:

- Packaged build: set `WISP_APP_BUNDLE` to the built `wisp.app` in the workflows, or make that path the default in `app-launch`.
- Development build, which Code - OSS runs as `.build/electron/<name>.app` with the source tree as its argument: replace `fromFixture()` in `app-launch` with a function that returns that executable, `args: ['<editor dir>']`, and an `env` with only the additions the build needs (upstream `scripts/code.sh` sets `VSCODE_DEV=1`, among others). Callers already merge `env`, so the spec does not change.

Then point `check-editor`, `build-app`, and `package-app` at the fork's commands and delete the fixture, as 0001 describes. Set the root `.nvmrc` to the fork's own `.nvmrc` (upstream pins an exact 24.x) and keep the two equal, because the scripts check against the root one.

## package-app (added by #5)

The fixture has no packaging, so there is no `.app` for `release.yml` yet. #5 adds `scripts/ci/package-app` with this contract, and #9 later points it at the fork:

- `scripts/ci/package-app <version>` builds `wisp.app` with `<version>` stamped in and prints the bundle's absolute path as the only line on stdout. That path goes straight into the zip step, and into `WISP_APP_BUNDLE` for a launch check.
- The version is stamped in the CI checkout only and never committed, so publishing never pushes to `main`.
- Stamping the fixture: run `npm version <version> --no-git-tag-version` in `ci/fixtures/electron-smoke/`. It updates `package.json` and `package-lock.json` together, so `npm ci` keeps working.
- Stamping `wispd`: after setting `version` in `[workspace.package]` in `Cargo.toml`, run `cargo update --workspace --offline`. Otherwise `Cargo.lock` keeps the old version, and every `--locked` build, `check-rust` included, fails with "cannot update the lock file".

## Notes for workflows

- `check-editor` never downloads the Electron binary. Electron 44 fetches it on first use rather than at install, and `build-app` fetches it explicitly.
- Worth caching: `~/.rustup/toolchains`, keyed on `rust-toolchain.toml`, because the runner's preinstalled stable never matches the pin; `~/.cargo/registry`, `~/.cargo/git`, and `target/`; `~/.npm`, keyed on `ci/fixtures/electron-smoke/package-lock.json`; and `~/Library/Caches/electron` for the Electron download.
