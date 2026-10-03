# 0050: plxd installs gh and starts its sign-in

- Status: accepted
- Date: 2026-10-03
- Issue: PLX-423

## Context

Pull requests go through the GitHub CLI on each host (PLX-318). Settings > Source control only reported `gh` (PLX-336): when it was missing it linked to cli.github.com, and when it was signed out it told the user to run `gh auth login` themselves. PR actions failed with the same advice. Ryan wants GitHub set up in a couple of clicks, on any host, without Homebrew: his nix-darwin config zaps Homebrew packages it doesn't list.

0004 says plxd never starts a sign-in. That stays true for the AI vendor CLIs. GitHub's device flow is its own supported path for third-party tools, and `gh` keeps the token.

## Decision

- **plxd installs `gh` into its data folder.** `github/install` reads the latest `cli/cli` release from GitHub's API, downloads the archive for the host's OS and arch (`gh_<v>_macOS_{arm64,amd64}.zip`, `gh_<v>_linux_{amd64,arm64}.tar.gz`, `gh_<v>_windows_{amd64,arm64}.zip`) and `gh_<v>_checksums.txt` with the system `curl`, checks the archive's SHA-256, and unpacks it with the system `tar`. macOS and Windows 10+ ship bsdtar, which reads zip. It goes in `<data>/tools/gh/`, with no HTTP, zip, or tar crate. Everything happens in a temp folder inside `tools/` that is renamed into place at the end, so a failure leaves nothing behind. Install is refused while a `gh` is found. It never touches Homebrew or any package manager.
- **The user's `gh` wins.** `<data>/tools/gh/bin` goes at the end of the `PATH` every process plxd starts gets. A `gh` the user installs (nix, brew, anything) is found first, and plxd's copy is only a fallback. Threads and plxd's PR actions find either one.
- **Install runs in the background.** The app gives every request 30 s, and the download can take longer. `github/install` starts the install and answers at once. `github/status` reports `installing`, then `managed` once plxd's copy is the one in use, or `setupNote` with the reason the install failed.
- **plxd starts `gh auth login`, for `gh` only.** `github/signIn` runs `gh auth login --hostname github.com --git-protocol https --web` with stdin closed and `GH_PROMPT_DISABLED` unset. Non-interactive, gh prints its one-time code and `https://github.com/login/device` to stderr (`! One-time code (XXXX-XXXX) copied to clipboard`, or `! First copy your one-time code: XXXX-XXXX` when the copy fails), then waits without asking for Enter. plxd answers with the code and URL and keeps the process, one per host, until it exits, the code's 15 minutes run out, or `github/signInCancel` kills its process group. `github/status` reports it as `signingIn`. plxd never reads or logs the token.
- **Git uses the sign-in.** After a sign-in that worked, plxd runs `gh auth setup-git`. If that fails, as it does with a read-only home-manager `~/.gitconfig`, the failure becomes `setupNote` and the sign-in still counts.
- **The app.** On a plxd with the `githubSetup` capability, Settings > Source control's GitHub row goes Install, Installing, Sign in, then the code with Copy and Open GitHub (the device page opens in this computer's browser, for an SSH host too), then Signed in as @login. It polls `github/status` every 2 s while an install or sign-in is pending. A PR action that fails with `ghUnavailable` offers Set up GitHub, which opens Source control on that host. On an older plxd, the row is unchanged.

## Consequences

- 0013's worker sandbox blocks reads of the data folder, so a coordinator's sandboxed workers can't run plxd's copy of `gh`. Threads (0034) and plxd's own PR actions can.
- plxd never updates its `gh`. Removing `<data>/tools/gh/` and installing again picks up the latest.
- On Windows, a GNU `tar` earlier on `PATH`, such as Git for Windows' `usr/bin`, can't read the zip. System32's bsdtar normally comes first.
- gh copies the code to the host's clipboard on its own. On an SSH host that is the remote clipboard, and the app's Copy button covers this computer's.
