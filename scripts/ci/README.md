# CI entry points

Workflows call these scripts instead of running cargo or npm themselves, so a local run is the same as a CI run. When the Code - OSS fork (#8) replaces the Electron fixture, these scripts change and the workflows do not. Background: [0001](../../docs/decisions/0001-ci-before-product-code.md).

Each script finds the repo root on its own, so it runs from any directory.

| Script | What it does | Called by |
| --- | --- | --- |
| `check-rust` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, and `cargo test` on the workspace, with `--locked` | `ci.yml` (#3) |
| `check-editor` | `npm ci`, then lint, type-check (`tsc --noEmit`), build, and test the editor, currently `ci/fixtures/electron-smoke/` | `ci.yml` (#3) |
| `build-app` | Installs, builds, and downloads the Electron binary: what `app-launch` needs, without lint or tests | `screenshots.yml` (#4) |
| `app-launch` | Prints Playwright `_electron.launch` options for the built app as one line of JSON | `screenshots.yml` (#4) |

## Requirements

- Rust: rustup. `rust-toolchain.toml` pins the toolchain and its components, and rustup installs them on first use.
- Node: the major version in the root `.nvmrc`. In Actions, use `actions/setup-node` with `node-version-file: .nvmrc`.
- macOS, for `build-app` and `app-launch`.

## app-launch

The output is one line with absolute paths:

```json
{"executablePath":"<repo>/ci/fixtures/electron-smoke/node_modules/electron/dist/Electron.app/Contents/MacOS/Electron","args":["<repo>/ci/fixtures/electron-smoke/out/main.js"]}
```

Pass it to Playwright unchanged:

```js
const options = JSON.parse(execFileSync('scripts/ci/app-launch', { encoding: 'utf8' }));
const electronApp = await _electron.launch(options);
const window = await electronApp.firstWindow();
```

- The fixture opens one window titled `wisp`.
- If the app is not built, `app-launch` explains why on stderr, prints nothing to stdout, and exits 1. Run `build-app` first.
- `WISP_APP_BUNDLE=/path/to/wisp.app` launches a built bundle instead: the bundle's `CFBundleExecutable`, with no arguments.
- Callers pass the output straight to `_electron.launch`, so every key must be one of its options.
- Playwright adds its startup hook only when it locates Electron itself. With `executablePath`, the app starts without waiting for Playwright, so wait for `firstWindow()` before inspecting windows with `electronApp.evaluate()`.

## Switching to the real app (#8)

When the fork builds, #8 does one of these:

- Packaged build: set `WISP_APP_BUNDLE` to the built `wisp.app` in the workflows, or make that path the default in `app-launch`.
- Development build, which Code - OSS runs as `.build/electron/<name>.app` with the source tree as its argument: replace `fromFixture()` in `app-launch` with a function that returns that executable, `args: ['<editor dir>']`, and any `env` the build needs (upstream `scripts/code.sh` sets `VSCODE_DEV=1`, among others).

Then point `check-editor` and `build-app` at the fork's commands and delete the fixture, as 0001 describes.

## Notes for workflows

- `check-editor` never downloads the Electron binary. Electron 44 fetches it on first use rather than at install, and `build-app` fetches it explicitly.
- Worth caching: `~/.cargo/registry`, `~/.cargo/git`, and `target/`; `~/.npm`, keyed on `ci/fixtures/electron-smoke/package-lock.json`; and `~/Library/Caches/electron` for the Electron download.
- There is no packaging for the fixture. #5 can package `ci/fixtures/electron-smoke/` after `build-app`, or wait for #9.
