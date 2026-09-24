# 0001: CI before product code, against a stand-in app

- Status: accepted
- Date: 2026-09-23
- Issue: #2

## Context

`CLAUDE.md` puts CI/CD before any product code, so every product PR is checked from its first commit. But `ci.yml` (#3), `screenshots.yml` (#4), and `release.yml` (#5) need something real to lint, build, test, launch, and screenshot, and neither `wispd` nor the Code - OSS fork (#8) exists yet. A workflow that has only ever run against nothing fails for the first time on the first product PR.

## Decision

- Scaffold the real Cargo workspace now. `daemon/` builds `wispd`, which handles only `--version` and `--help` and has unit and integration tests. It is the start of the daemon, not a fixture, and it stays.
- Add a throwaway Electron app, `ci/fixtures/electron-smoke/`, that stands in for the editor: strict TypeScript, ESLint, unit tests, and one window titled `wisp`. It has no packaging.
- Workflows reach both parts only through `scripts/ci/`: `check-rust`, `check-editor`, `build-app`, and `app-launch`, which prints Playwright `_electron.launch` options. Replacing the fixture with the fork changes these scripts, not the workflows.

## Consequences

- #3, #4, and #5 can be written and proven green before any product code exists.
- A green screenshot job proves the pipeline, not the editor. Until the fork lands, the screenshots show the fixture.
- The fixture's Electron, TypeScript, and ESLint versions need occasional updates until it is removed.
- `scripts/ci/` is an interface. A PR that changes what a script does or prints also updates the workflows that call it.

## Removal

Delete `ci/fixtures/electron-smoke/` once #8 makes the real app launchable through `scripts/ci/app-launch` and `screenshots.yml` (#4) captures it. The same change points `check-editor` and `build-app` at the fork. The workflows keep calling the same scripts; only cache keys that name the fixture's lockfile change.
