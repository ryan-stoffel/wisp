# wisp

An open-source macOS app that reproduces the Cursor Projects workflow on machines you own. One coordinator chat plans the work and spawns subagents that share project context. Model calls go through your own AI subscriptions, managed inside the app, with raw API keys as a fallback.

wisp has two parts:

- An editor app built on a stripped-down fork of VS Code (Code - OSS)
- `wispd`, a Rust daemon that runs the coordinator, triggers, and project state on the host. [daemon/README.md](daemon/README.md) covers its commands and how to set up a Mac as a host over SSH.

Status: early development. See [docs/PLAN.md](docs/PLAN.md) for the plan and milestones.

## Install

wisp needs macOS 13 or later on a Mac with Apple silicon. Install it with Homebrew:

```sh
brew install --cask ryan-stoffel/taps/wisp
```

Installing by the full name trusts only this cask, not the whole tap. Since Homebrew 6, third-party taps have to be trusted explicitly. Until the editor fork is done (milestone M0), a release opens only a placeholder window.

### Opening wisp the first time

wisp is not signed with an Apple Developer ID yet (#7), so macOS blocks it the first time it opens. Homebrew marks every app it downloads as quarantined, and Homebrew 7 removed the `--no-quarantine` option, so you cannot skip this at install time. Use one of these:

- **System Settings**:
  1. Open wisp from Applications. macOS refuses to open it. Close the warning, and don't move wisp to the Trash.
  2. Open System Settings > Privacy & Security and scroll to Security, where wisp is listed as blocked. Click Open Anyway. The button is there for about an hour after you try to open the app.
  3. Enter your login password to confirm.
- **Terminal**: remove the quarantine attribute, then open wisp as usual:

  ```sh
  xattr -dr com.apple.quarantine /Applications/wisp.app
  ```

On macOS 15 and later, Control-click > Open no longer gets past this check.

macOS asks again after every `brew upgrade --cask wisp`, because each build has a new ad-hoc signature.

To uninstall, run `brew uninstall --cask wisp`. Add `--zap` to also delete wisp's settings and data in `~/Library`.

## License

[Apache-2.0](LICENSE)
