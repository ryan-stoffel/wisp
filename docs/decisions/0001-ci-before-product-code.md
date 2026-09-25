# 0001: CI before product code, against a stand-in app

- Status: accepted; the stand-in app was retired in #38 (see [Retired](#retired))
- Date: 2026-09-23
- Issue: #2

## Context

`CLAUDE.md` puts CI/CD before any product code, so every product PR is checked from its first commit. But `ci.yml` (#3), `screenshots.yml` (#4), and `release.yml` (#5) need something real to lint, build, test, launch, and screenshot, and neither `wispd` nor the Code - OSS fork (#8) exists yet. A workflow that has only ever run against nothing fails for the first time on the first product PR.

## Decision

- Scaffold the real Cargo workspace now. `daemon/` builds `wispd`, which handles only `--version` and `--help` and has unit and integration tests. It is the start of the daemon, not a fixture, and it stays.
- Add a throwaway Electron app, `ci/fixtures/electron-smoke/`, that stands in for the editor: strict TypeScript, ESLint, unit tests, and one window titled `wisp`. It has no packaging yet.
- Workflows reach both parts only through `scripts/ci/`: `check-rust`, `check-editor`, `build-app`, and `app-launch`, which prints Playwright `_electron.launch` options. #5 adds `scripts/ci/package-app`, which builds `wisp.app` with a stamped version; its contract is in `scripts/ci/README.md`. Replacing the fixture with the fork changes these scripts, not the workflows.

## Consequences

- #3 and #4 can be written and proven green before any product code exists. #5 cannot be proven green until it adds `scripts/ci/package-app`, because until then there is no `.app` to package.
- A green screenshot job proves the pipeline, not the editor. Until the fork lands, the screenshots show the fixture.
- The fixture's Electron, TypeScript, and ESLint versions need occasional updates until it is removed.
- `scripts/ci/` is an interface. A PR that changes what a script does or prints also updates the workflows that call it.

## Removal

Delete `ci/fixtures/electron-smoke/` once #8 makes the real app launchable through `scripts/ci/app-launch` and `screenshots.yml` (#4) captures it. The same change points `check-editor`, `build-app`, and `package-app` at the fork. The workflows keep calling the same scripts; only cache keys that name the fixture's lockfile change.

## Retired

#38 deleted the fixture on 2026-09-24, once #9 had a packaged `Wisp.app` and a cached build of it in `ci.yml`'s `app` job. It went differently from the plan above in four ways:

- **`build-app` and `app-launch`** build and launch the packaged `Wisp.app`. `app-launch` launches `WISP_APP_BUNDLE`, or else what `build-app` built. The `WISP_APP` switch and the development build are gone from the CI scripts.
- **`screenshots.yml` changed too.** Its `capture` job restores the `app (arm64)` job's cached zip under the same key, from `scripts/ci/app-cache-key`, and builds the app itself only on a miss. `scripts/ci/README.md` explains why it builds rather than waiting for `ci.yml`.
- **`check-editor` was deleted, not repointed.** What wisp owns in the editor is checked by `check-fork` in the `fork` job (the patches and overlay apply, and `src/` type-checks) and by `check-app` in the `app` job (the app builds and is branded). The `editor` job became `screenshots`, which runs `check-screenshots` (#39). Linting and unit-testing wisp's own editor TypeScript is #88.
- **`package-app` still names the fixture**, so it and the release dry run fail until #43 points them at the fork.
