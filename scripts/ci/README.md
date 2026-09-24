# CI entry points

Workflows call these scripts instead of running cargo or npm themselves, so a local run is the same as a CI run. When the Code - OSS fork (#8) replaces the Electron fixture, these scripts change and the workflows do not. Background: [0001](../../docs/decisions/0001-ci-before-product-code.md).

Each script finds the repo root on its own, so it runs from any directory.

| Script | What it does | Called by |
| --- | --- | --- |
| `check-rust` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, and `cargo test` on the workspace, with `--locked` | `ci.yml` (#3) |
| `check-editor` | `npm ci`, then lint, type-check (`tsc --noEmit`), build, and test the editor, currently `ci/fixtures/electron-smoke/` | `ci.yml` (#3) |
| `build-app` | Installs, builds, and downloads the Electron binary: what `app-launch` needs, without lint or tests | `screenshots.yml` (#4) |
| `app-launch` | Prints Playwright `_electron.launch` options for the built app as one line of JSON | `screenshots.yml` (#4) |
| `package-app` | Builds `wisp.app` with a version stamped in, ad-hoc signs it, zips it, and prints the bundle path (see [package-app](#package-app)) | `release.yml` (#5) |
| `next-version` | Prints the version the next release gets, from tags and Conventional Commits (see [Releases](#releases)) | `release.yml` (#5) |
| `generate-cask` | Prints the Homebrew cask for a version and its zips, from `release/wisp.rb.template` | `release.yml` (#5) |
| `audit-cask` | Runs `brew style` and `brew audit` on a cask in a throwaway tap, then installs and uninstalls it | `release.yml` (#5) |
| `publish-cask` | Commits the cask to `ryan-stoffel/homebrew-taps` with `TAP_GITHUB_TOKEN`; `--check` only tests the token | `release.yml` (#5) |
| `check-release` | Unit tests for `next-version` and `generate-cask` | `release.yml` (#5) |

## Requirements

- Rust: rustup. `rust-toolchain.toml` pins the toolchain and its components. In CI, run `rustup toolchain install` with no arguments as its own step before `check-rust`. It installs exactly what the file pins, and it does not rely on rustup's auto-install, which can be turned off. Locally, rustup installs the pin on first use.
- Node: the exact version in the root `.nvmrc`. In Actions, use `actions/setup-node` with `node-version-file: .nvmrc`. `check-editor` and `build-app` stop with an error when `node` has a different major version, so local runs use the same Node as CI.
- macOS, for `build-app`, `app-launch`, and `package-app`.
- Homebrew, for `audit-cask`. GitHub's macOS runners have it.

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

## Switching to the real app (#8)

When the fork builds, #8 does one of these:

- Packaged build: set `WISP_APP_BUNDLE` to the built `wisp.app` in the workflows, or make that path the default in `app-launch`.
- Development build, which Code - OSS runs as `.build/electron/<name>.app` with the source tree as its argument: replace `fromFixture()` in `app-launch` with a function that returns that executable, `args: ['<editor dir>']`, and an `env` with only the additions the build needs (upstream `scripts/code.sh` sets `VSCODE_DEV=1`, among others). Callers already merge `env`, so the spec does not change.

Then point `check-editor`, `build-app`, and `package-app` at the fork's commands and delete the fixture, as 0001 describes. Set the root `.nvmrc` to the fork's own `.nvmrc` (upstream pins an exact 24.x) and keep the two equal, because the scripts check against the root one.

## package-app

`scripts/ci/package-app <version> [arm64|x64]` packages for this Mac's architecture unless you name one. It prints the bundle's absolute path as the only line on stdout, for `WISP_APP_BUNDLE`, and sends everything else to stderr. Steps:

1. Stamp `<version>` into the fixture with `npm version <version> --no-git-tag-version --allow-same-version`, which updates `package.json` and `package-lock.json` together, and set `productName` to `wisp`, so the app keeps its data in `~/Library/Application Support/wisp` ([0003](../../docs/decisions/0003-naming.md)). Both files are restored on exit, so the version is never committed and publishing never pushes to `main`.
2. Run `build-app`, then `@electron/packager`, into `dist/wisp-darwin-<arch>/wisp.app` with the bundle id `io.github.ryan-stoffel.wisp`.
3. Ad-hoc sign the whole bundle and check its signature, version, and bundle id. Packager keeps Electron's per-binary signatures, which no longer match the renamed bundle. Gatekeeper then reports the app as damaged and offers no Open Anyway. #7 replaces this step with Developer ID signing and notarization.
4. Zip the bundle with `ditto` to `dist/wisp-<version>-<arch>.zip`, the name the cask's `url` expects.

When #9 points it at the fork, only steps 1 and 2 change. Once `wispd` ships inside the bundle, stamp it too: after setting `version` in `[workspace.package]` in `Cargo.toml`, run `cargo update --workspace --offline`. Otherwise `Cargo.lock` keeps the old version, and every `--locked` build, `check-rust` included, fails with "cannot update the lock file".

## Releases

`release.yml` runs only for PRs whose head is `develop` and whose base is `main`. Background: [0006](../../docs/decisions/0006-release-versioning-and-packaging.md).

- **While the PR is open**, `dry-run` runs the steps below and publishes nothing. The job summary shows the version the merge would release, the zip's sha256, and the cask. The zip and cask are also attached as the `wisp-<version>-dry-run` artifact.
- **When Ryan merges it**, `publish` does the following:
  1. Fails first if the `TAP_GITHUB_TOKEN` secret is missing or cannot read the tap (#28), before anything is tagged.
  2. Repeats the same steps on the merge commit.
  3. Runs `gh release create v<version> --target <merge commit> --generate-notes` with the zips, which also creates the tag.
  4. Runs `publish-cask` to commit `Casks/wisp.rb` to the tap's default branch as `wisp <version>`.

  Releases queue rather than overlap. Merge release PRs with a merge commit (CLAUDE.md). After a squash merge, the next version would count all of `develop`'s history.

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

If `publish` fails after the release exists, re-run it. It reuses the tag and the release's zips, and `publish-cask` skips the commit when the tap already has the same cask. If a failed run left a draft release, delete the draft first.

## Notes for workflows

- `check-editor` never downloads the Electron binary. Electron 44 fetches it on first use rather than at install, and `build-app` fetches it explicitly.
- Worth caching: `~/.rustup/toolchains`, keyed on `rust-toolchain.toml`, because the runner's preinstalled stable never matches the pin; `~/.cargo/registry`, `~/.cargo/git`, and `target/`; `~/.npm`, keyed on `ci/fixtures/electron-smoke/package-lock.json`; and `~/Library/Caches/electron` for the Electron download.
