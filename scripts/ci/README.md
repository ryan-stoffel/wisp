# CI entry points

Workflows call these scripts instead of running cargo or npm themselves, so a local run is the same as a CI run. When the Code - OSS fork (#8) replaces the Electron fixture, these scripts change and the workflows do not. Background: [0001](../../docs/decisions/0001-ci-before-product-code.md).

Each script finds the repo root on its own, so it runs from any directory.

| Script | What it does | Called by |
| --- | --- | --- |
| `check-rust` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, and `cargo test` on the workspace, with `--locked` | `ci.yml` (#3) |
| `check-editor` | Checks that the Code - OSS pin and patches still apply (`scripts/editor/prepare`, then `scripts/editor/export-patches --check`) and that the root `.nvmrc` equals upstream's. Then `npm ci`, lint, type-check (`tsc --noEmit`), build, and test the fixture, `ci/fixtures/electron-smoke/` | `ci.yml` (#3) |
| `build-app` | Installs, builds, and downloads the Electron binary: what `app-launch` needs, without lint or tests. `WISP_APP=editor` builds the Code - OSS development build in `editor/vscode/` instead of the fixture. | `screenshots.yml` (#4) |
| `app-launch` | Prints Playwright `_electron.launch` options for the built app as one line of JSON. `WISP_APP=editor` selects the Code - OSS development build. | `screenshots.yml` (#4) |
| `package-app` | Not yet written: #5 adds it (see [package-app](#package-app-added-by-5)) | `release.yml` (#5) |

## Requirements

- Rust: rustup. `rust-toolchain.toml` pins the toolchain and its components. In CI, run `rustup toolchain install` with no arguments as its own step before `check-rust`. It installs exactly what the file pins, and it does not rely on rustup's auto-install, which can be turned off. Locally, rustup installs the pin on first use.
- Node: the exact version in the root `.nvmrc`. It always equals upstream's `.nvmrc` at the pinned Code - OSS release: `scripts/editor/upgrade` copies it, and `check-editor` fails if the two differ. In Actions, use `actions/setup-node` with `node-version-file: .nvmrc`. `check-editor` and `build-app` stop with an error when `node` has a different major version, so local runs use the same Node as CI.
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
- If the app is not built, `app-launch` explains why on stderr, prints nothing to stdout, and exits 1. Run `build-app` first, with the same `WISP_APP`.
- `WISP_APP_BUNDLE=/path/to/wisp.app` launches a built bundle instead: the bundle's `CFBundleExecutable`, with no arguments.
- Every key in the output is an `_electron.launch` option. The fixture prints no `env`. The editor prints only additions, and the merge above keeps `PATH`, `HOME`, and the rest.
- Playwright adds its startup hook only when it locates Electron itself. With `executablePath`, the app starts without waiting for Playwright, so wait for `firstWindow()` before inspecting windows with `electronApp.evaluate()`.

### The Code - OSS development build

`WISP_APP=editor` prints the development build in `editor/vscode/` ([0002](../../docs/decisions/0002-editor-fork-strategy.md)). Build it with `WISP_APP=editor scripts/ci/build-app`. The executable is the Electron app that upstream downloads into `.build/electron/`. The source tree is its first argument, and `env` holds the variables that upstream's `scripts/code.sh` sets:

```json
{"executablePath":"<repo>/editor/vscode/.build/electron/Code - OSS.app/Contents/MacOS/Code - OSS","args":["<repo>/editor/vscode","--disable-extension=vscode.vscode-api-tests"],"env":{"NODE_ENV":"development","VSCODE_DEV":"1","VSCODE_CLI":"1","ELECTRON_ENABLE_STACK_DUMPING":"1","ELECTRON_ENABLE_LOGGING":"1"}}
```

- Add arguments after the printed ones, and keep the source tree first. Add a folder or file to open. For repeatable screenshots, also add a fresh `--user-data-dir=<dir>` and `--extensions-dir=<dir>`, because the default profile is shared with any other Code - OSS development build on the machine.

## Switching to the real app

#8 imported the fork. It added the development build to `build-app` and `app-launch` behind `WISP_APP=editor`, with the fixture still the default, made `check-editor` check the fork's pin and patches, and set the root `.nvmrc` to upstream's.

The fixture stays the default until #9 makes a cached fork build fast enough for every PR. Then #38:

- Makes the editor the default in `build-app` and `app-launch`, or sets `WISP_APP_BUNDLE` to the packaged `wisp.app`.
- Points `check-editor` and `package-app` at the fork's commands.
- Deletes the fixture, as 0001 describes.

## package-app (added by #5)

The fixture has no packaging, so there is no `.app` for `release.yml` yet. #5 adds `scripts/ci/package-app` with this contract, and #9 later points it at the fork:

- `scripts/ci/package-app <version>` builds `wisp.app` with `<version>` stamped in and prints the bundle's absolute path as the only line on stdout. That path goes straight into the zip step, and into `WISP_APP_BUNDLE` for a launch check.
- The version is stamped in the CI checkout only and never committed, so publishing never pushes to `main`.
- Stamping the fixture: run `npm version <version> --no-git-tag-version` in `ci/fixtures/electron-smoke/`. It updates `package.json` and `package-lock.json` together, so `npm ci` keeps working.
- Stamping `wispd`: after setting `version` in `[workspace.package]` in `Cargo.toml`, run `cargo update --workspace --offline`. Otherwise `Cargo.lock` keeps the old version, and every `--locked` build, `check-rust` included, fails with "cannot update the lock file".

## Notes for workflows

- `check-editor` never downloads the Electron binary. Electron 44 fetches it on first use rather than at install, and `build-app` fetches it explicitly.
- Worth caching: `~/.rustup/toolchains`, keyed on `rust-toolchain.toml`, because the runner's preinstalled stable never matches the pin; `~/.cargo/registry`, `~/.cargo/git`, and `target/`; `~/.npm`, keyed on `ci/fixtures/electron-smoke/package-lock.json`; and `~/Library/Caches/electron` for the Electron download.
- Building the editor (`WISP_APP=editor`) is dominated by `npm ci`: 2 min 20 s cold and 2 min with a warm `~/.npm` on an M3 Pro. Most of that is native module builds and install scripts, so caching the installed `node_modules` directories saves more than caching `~/.npm`. #8's Progress comment has the full numbers for #9.
- `npm run download-builtin-extensions` calls the GitHub REST API. Pass `GITHUB_TOKEN` so that it does not share the anonymous rate limit.
