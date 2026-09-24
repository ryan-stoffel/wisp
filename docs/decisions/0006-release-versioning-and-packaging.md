# 0006: Release versions, packaging, and signing

- Status: accepted
- Date: 2026-09-24
- Issue: #5

## Context

`release.yml` (#5) ships wisp through the Homebrew cask `ryan-stoffel/taps/wisp` before the Code - OSS app (#9), a bundled `wispd` (M1), or an Apple Developer ID (#7) exist. Some choices it makes bind those later issues: where the version comes from, which architectures ship, how the unsigned app is signed, and the bundle identifier and data folder that the cask's `zap` stanza names.

## Decision

- **The version lives only in the `vX.Y.Z` tag on the release merge commit in `main`.**
  - `scripts/ci/next-version` computes it. The first release is `0.1.0`. After that, the Conventional Commits since the last tag decide the bump: a breaking change is a major bump, or a minor bump before 1.0; `feat` is a minor bump; anything else is a patch bump.
  - `scripts/ci/package-app` stamps the version into the build checkout and never commits it. The versions in `package.json` and `Cargo.toml` are placeholders.
- **Releases ship arm64 only, as `wisp-<version>-arm64.zip`.** macOS 27 runs only on Apple Silicon. Homebrew moved Intel macOS to Tier 3 in September 2026 and removes it in September 2027 (`docs/Support-Tiers.md` in Homebrew 7.0.6). Adding `x64` to `RELEASE_ARCHES` in `release.yml` ships an Intel zip too, and the cask template already covers both.
- **Until #7, the bundle is ad-hoc signed rather than left as `@electron/packager` produces it.** Packager's output fails `codesign --verify`, and Gatekeeper calls such an app damaged, with no Open Anyway. An ad-hoc signed app fails only the Developer ID check, which Open Anyway accepts. The README explains how to open it.
- **The bundle identifier is `io.github.ryan-stoffel.wisp`**, because the project has no domain of its own. **App data lives in `~/Library/Application Support/wisp`** (0003). The cask's `zap` stanza lists these paths, so the fork's `product.json` (#9) must use the same identifier and data folder, or change the cask with them.

## Consequences

- Publishing never pushes to `main`, and the versions in the repo never have to change for a release.
- Until #7, macOS asks for approval after every upgrade. Each build has a new ad-hoc signature, whose designated requirement is the build's own hash, so Homebrew cannot carry the user's earlier approval over to the new version.
- #9 changes only the fixture half of `package-app` (stamping and packaging). It changes `RELEASE_ARCHES` only if Intel builds should ship.
- Release PRs must be merged with a merge commit, as CLAUDE.md already says. After a squash merge, the next version would count the wrong commits. `release.yml` checks that the merge commit's second parent is the PR's head, and otherwise releases nothing.
- Hotfix PRs into `main` publish nothing. Their changes ship with the next release from `develop`.
