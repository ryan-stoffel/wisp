# Working on the editor and upgrading Code - OSS

wisp's editor is upstream Code - OSS at the release pinned in `editor/upstream.json`, plus wisp's overlay and the patches in `editor/patches/`. [0002](decisions/0002-editor-fork-strategy.md) explains why, and [0008](decisions/0008-editor-overlay.md) explains the overlay. This page covers building the editor, changing wisp's patches and overlay, and moving to a newer upstream release.

## Layout

| Path | What it is | In git |
| --- | --- | --- |
| `editor/upstream.json` | The pin: upstream repository, release tag, and the commit the tag must resolve to | Yes |
| `editor/product.json` | wisp's changes to upstream's `product.json`, as a JSON merge patch: branding, Open VSX, telemetry, updates, and removed Copilot and debugger keys | Yes |
| `editor/overlay/` | Files copied into the tree at the same path, such as the app icon `resources/darwin/code.icns` | Yes |
| `editor/branding/icon.svg` | The source of the app icon. `scripts/editor/make-icon` renders it into `editor/overlay/`. | Yes |
| `editor/patches/NNNN-*.patch` | wisp's changes to upstream files, one `git format-patch` file per commit, applied in filename order | Yes |
| `editor/vscode/` | Upstream at the pin, then one commit with the overlay, then the patches as commits, on the branch `wisp`. It also holds `node_modules`, `out`, and `.build` after a build. | No |
| `editor/VSCode-darwin-<arch>/` | The packaged app from `scripts/editor/build-app` | No |
| `scripts/editor/prepare` | Creates or updates `editor/vscode/` from the pin, the overlay, and the patches | Yes |
| `scripts/editor/export-patches` | Writes the commits in `editor/vscode/` back to `editor/patches/` | Yes |
| `scripts/editor/upgrade <tag>` | Rebases the patches onto another upstream release and updates the pin | Yes |
| `scripts/editor/build-app [arm64\|x64]` | Builds the packaged `Wisp.app` | Yes |
| `scripts/editor/test` | Tests `prepare`, `export-patches`, and `upgrade` against a local stand-in for upstream, without network access. Run it after changing them. CI runs it in `check-fork`. | Yes |

`prepare` never overwrites work. It stops if `editor/vscode/` has uncommitted changes, untracked files, or commits that were not exported. `--force` discards them.

## Prerequisites

- macOS with Xcode and its command line tools. Native modules build with clang.
- Python 3, for `node-gyp`.
- git 2.34 or later.
- Node as pinned in the root `.nvmrc`, which always equals upstream's `editor/vscode/.nvmrc` (24.18.0 for 1.139.0). Upstream's `npm ci` refuses an older Node or another major version. With fnm or nvm, run `fnm use` or `nvm use` in the repo root.
- About 10 GB of free disk: 7.5 GB for `editor/vscode/` after a build, plus about 1.6 GB in the npm, node-gyp, Electron, and Playwright caches. A packaged build adds about 1.3 GB per architecture for the app and its zip.

## Build and run

```sh
scripts/editor/prepare
cd editor/vscode
npm ci                                  # 2 min 20 s cold, 2 min warm
npm run electron                        # 10 s
npm run compile                         # 27 s; or npm run watch while you work
npm run download-builtin-extensions     # 5 s
./scripts/code.sh                       # the development build
```

The times are from an M3 Pro. CI does not use the development build: its scripts build and launch the packaged app below.

`npm ci` also downloads Playwright's Chromium (550 MB) for upstream's browser tests. Set `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` to skip it when you do not run those tests. `build-app` sets it.

The development build keeps upstream's `code-oss-dev` user data folder, which upstream hard-codes for any build run from source. Only the packaged app uses wisp's folders.

### The packaged app

```sh
scripts/editor/build-app          # this Mac's architecture, or: build-app arm64, build-app x64
```

`build-app` runs `prepare`, then `npm ci` for the target architecture, unless `node_modules` already matches it. Then it downloads the built-in extensions, runs upstream's `gulp vscode-darwin-<arch>-min`, ad-hoc signs the bundle, and checks that every Mach-O file is built for that architecture. It prints the bundle's path, `editor/VSCode-darwin-<arch>/Wisp.app`. `scripts/ci/check-app <app or zip> <arch>` checks the bundle's branding, signature, and launcher, which CI's `app` job also runs. `scripts/ci/screenshots --request <scenes>` captures the scenes a PR asks CI for from that bundle ([scripts/ci/README.md](../scripts/ci/README.md#screenshots)).

- **Launcher**: `Wisp.app/Contents/Resources/app/bin/wisp`. `wisp <folder>` opens the folder in the app. The command palette's **Shell Command: Install 'wisp' command in PATH** links `/usr/local/bin/wisp` to it. The Homebrew cask links it into Homebrew's `bin` instead.
- **Version**: upstream's, 1.139.0, unless `--app-version <X.Y.Z>` names another. `scripts/ci/package-app` passes the release version, which then shows in the About dialog, in `wisp --version`, and in `Info.plist`. `build-app` sets it in `editor/vscode/package.json` only while gulp runs.
- **x64** builds on Apple silicon, as in upstream's pipeline: `npm_config_arch=x64` makes npm build the native modules for x64. Switching architectures reinstalls `node_modules`, which takes about 2 minutes.
- **Time**: on an M3 Pro, `npm ci` 2 min, gulp 3 min 30 s, and signing and checks about 1 min. On a `macos-26` runner, each architecture takes about 20 minutes (see [scripts/ci/README.md](../scripts/ci/README.md#the-app-job)).
- **Size**: `du -sh` reports 921M for the arm64 app and 979M for x64. Their zips are 321 MB and 346 MB.
- **State**:
  - User data goes to `~/Library/Application Support/Wisp`, extensions and `argv.json` to `~/.wisp`, and shared storage to `~/.wisp-shared`.
  - macOS keeps its own files under the bundle id `io.github.ryan-stoffel.wisp`.
  - None of this is shared with VS Code or Code - OSS. That includes the device id that Microsoft's developer tools share: patch 0006 keeps wisp out of it.

## Change product.json or the overlay

- `editor/product.json` is merged into upstream's `product.json` as a [JSON merge patch](https://www.rfc-editor.org/rfc/rfc7396): objects merge key by key, and `null` deletes a key.
- A file under `editor/overlay/` lands at the same path in the tree, replacing upstream's file if there is one.
- To change the app icon, edit `editor/branding/icon.svg` and run `scripts/editor/make-icon`. It writes `editor/overlay/resources/darwin/code.icns`.

Make these changes in wisp, not in `editor/vscode/`, then run `scripts/editor/prepare`. It rebuilds the overlay commit and reapplies the patches in a second or two. `export-patches` refuses a patch that changes `product.json` or an overlaid file. [0008](decisions/0008-editor-overlay.md) explains why.

## What wisp leaves out

wisp opens into upstream's Agents window, its main UI, and keeps the editor window for quick edits ([0011](decisions/0011-agents-window-baseline.md)). The two are separate workbenches: the editor window starts from `src/vs/workbench/workbench.desktop.main.ts`, and the Agents window from `src/vs/sessions/sessions.desktop.main.ts`. Both register through the same view container, workbench contribution, and action registries, so one set of exclusion lists covers both.

- **Editor window.** #10 strips it to the editor, the file tree, search, source control, and the terminal. Its inventory of upstream's built-in extensions and workbench contributions is in [#10](https://github.com/ryan-stoffel/wisp/issues/10). AI features stay off there.
- **Agents window.** #103 leaves upstream's sessions layer, the chat stack it renders sessions with, and the sessions services. It takes out everything Copilot- or Microsoft-shaped: the sign-in gate, the account button, AI Customizations, Automations, upstream's Sessions list (wisp's sidebar replaces it, #12), the Copilot Chat sessions provider, the local agent host and the utility process it starts, remote agent hosts (SSH, dev tunnels, WSL, Dev Containers, WebSockets, GitHub cloud sandboxes), the GitHub pull request integration, onboarding tours, and voice. The inventory is in [#103](https://github.com/ryan-stoffel/wisp/issues/103).

The removals live in these places:

| Where | What it removes |
| --- | --- |
| `editor/overlay/src/vs/workbench/common/wisp/exclusions.ts` | View containers and workbench contributions, by id, grouped by feature with a reason for each. Editor window: upstream Chat's container, Run and Debug, the Debug Console, Ports, chat setup, the Copilot status item, and the remote indicator. Agents window: the Agents window parts listed above. The `strip: skip the view containers...` patch makes the registries skip them, so they are never registered. |
| `editor/overlay/src/vs/platform/wisp/common/excludedActions.ts` | Actions, by id, for features whose visible part is an action: the Agents window's account button, and the agent host's and voice mode's developer commands. The `strip: skip the actions...` patch makes `registerAction2` skip them, with their menu entries and keybindings. It sits in `platform` because `registerAction2` does. |
| `editor/overlay/src/vs/workbench/contrib/wisp/browser/wisp.contribution.ts` | Editor window default settings, registered in code so the first launch gets them: `chat.disableAIFeatures` is on, which hides the rest of upstream's Chat and Copilot entry points |
| `editor/overlay/src/vs/workbench/contrib/wisp/electron-browser/wispd.contribution.ts` | The wispd connection in either window: it registers `IWispdService` (served by the shared process), the `wisp.wispdState` and `wisp.wispdCapabilities` context keys, and the Show wispd Status and Reconnect to wispd commands. The editor window's `wisp.contribution.ts` and the Agents window's entry point both import it. |
| `editor/overlay/src/vs/sessions/contrib/wisp/electron-browser/wisp.sessions.contribution.ts` | wisp's entry point in the Agents window, loaded by the `workbench: load wisp in the Agents window` patch. It imports `wispd.contribution.ts` and the browser file below. |
| `editor/overlay/src/vs/sessions/contrib/wisp/browser/wisp.sessions.contribution.ts` | wisp's UI in the Agents window: the sessions provider, the sidebar view with its host chip, the no-host view, and the host menu. Its default settings turn off remote agent hosts (`chat.remoteAgentHostsEnabled`) and voice mode (`agents.voice.enabled`). The Agents window turns `chat.disableAIFeatures` off at its own workspace scope, which the editor window never reads. |
| `editor/product.json` | The js-debug extensions (`builtInExtensions`), Copilot's GitHub token grant (`trustedExtensionAuthAccess`), the voice endpoint (`voiceWsUrl`), and Copilot Chat's extension id (`defaultChatAgent.chatExtensionId`, empty). With no chat extension id, the Agents window skips its "Sign in to use Agents" gate. |
| The `strip:` patches | The Copilot extension and seven other built-in extensions in the packaged app (`build/lib/extensions.ts` lists them), the Accounts entry (hidden by default), and the Welcome page's "Connect to..." |

To remove another container, contribution, or action, add its id to `exclusions.ts` or `excludedActions.ts` with a one-line reason. `check-fork` fails if an id in either file no longer appears in upstream's source, because an upgrade that renames one would bring the feature back without an error. The development build still loads every folder in `extensions/`, because upstream scans the folder when running from source; only the packaged app leaves out the extensions above.

### How wisp opens

The `startup: open the Agents window at launch` patch changes two places in `src/vs/code/electron-main/app.ts`:

- Started with no file or folder arguments, no `--new-window`, and no macOS open-file event, wisp opens the Agents window instead of restoring or opening an editor window. `--remote` keeps upstream's behavior.
- When the dock reopens the app with no visible window, it opens the Agents window instead of an empty editor window.

`wisp <folder>`, `wisp <file>`, and `wisp --new-window` open the editor window, and `wisp --agents` opens the Agents window. The Agents window's **IDE** button (upstream's **Open in Editor**, relabeled by the `branding: call Open in Editor IDE` patch) opens the current session's folder in the editor window.

## Change a patch or add one

1. Run `scripts/editor/prepare`.
2. Edit, build, and test in `editor/vscode/`.
3. Commit there.
   - For a new patch, make a new commit. The subject becomes the file name, so start it with the area (`branding:`, `strip:`, `chat:`). The body says why wisp needs the change.
   - To change an existing patch, commit with `git commit --fixup=<commit>`, then fold it in with `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash refs/wisp/base`. `refs/wisp/base` is the overlay commit that the patches sit on. `git log` in `editor/vscode/` shows which commit is which patch.
4. Run `scripts/editor/export-patches`. It rewrites `editor/patches/` from the commits.
5. Commit `editor/patches/` in wisp. CI runs `export-patches --check`, which fails if the committed files differ from what an export would write, for example after a hand edit. It also type-checks `src/` in the patched tree.

Keep patches cheap to carry across upgrades. [0002](decisions/0002-editor-fork-strategy.md#rules-for-patches) has the rules. In short:

- Prefer configuration or new files over edits to upstream files.
- Do not patch lockfiles.
- Strip features by excluding them, not by deleting files.
- Stay out of upstream's chat and sessions code except for one-line imports, exclusions, and string changes, each its own patch ([0011](decisions/0011-agents-window-baseline.md#what-changes-in-0002)).

## Upgrade to a new upstream release

Move to the newest stable release about every four weeks, and within a week when an upstream release fixes an Electron or Chromium security issue. Open an issue for the upgrade and work on its `chore/` branch.

1. **Pick the release.** The first entry of `curl -s https://update.code.visualstudio.com/api/releases/stable` is the newest stable version, and its tag has the same name. Read its release notes at `https://code.visualstudio.com/updates/v1_<minor>`, and check its Electron and Node versions. The tag is not fetched yet, so read them from `https://raw.githubusercontent.com/microsoft/vscode/<tag>/.npmrc` (`target`) and `https://raw.githubusercontent.com/microsoft/vscode/<tag>/.nvmrc`.
2. **Prepare the current pin.**

   ```sh
   scripts/editor/prepare
   ```

3. **Rebase the patches.**

   ```sh
   scripts/editor/upgrade 1.140.0
   ```

   The script fetches the tag, makes wisp's overlay commit on it, and rebases the patch commits in `editor/vscode/` onto that overlay commit. Then it rewrites `editor/upstream.json`, copies upstream's `.nvmrc` to the root `.nvmrc`, and exports the patches.

   If a patch conflicts, the rebase stops. In `editor/vscode/`:

   - Run `git status` to see the conflicted files.
   - Read what upstream changed there with `git diff <old tag> <new tag> -- <file>`. Both tags are in the tree, but its history is shallow, so `git log` shows nothing useful.
   - Fix the files so they keep the patch's intent, then `git add` them.
   - Run `git rebase --continue`, and repeat until the rebase finishes. If upstream now does what a patch did, drop the patch with `git rebase --skip`.

   Then run `scripts/editor/upgrade 1.140.0` again to finish. To give up, run `git rebase --abort` in `editor/vscode/`, then `scripts/editor/prepare --force`.
4. **Check the series against a clean fetch.**

   ```sh
   scripts/editor/prepare
   scripts/editor/export-patches --check
   ```

5. **Rebuild and smoke-test.** Run `fnm use` or `nvm use` in the repo root first, because `upgrade` may have changed `.nvmrc`, and upstream's `npm ci` rejects an older Node. Then run the build steps above and launch the development build. Open a folder, open a file from the tree, edit and save it, and open a terminal. `npm ci` is needed because upstream's lockfile changes with almost every release. Once #13 lands, run its Playwright smoke tests too. If Electron's major version changed, build the packaged app and read `LSMinimumSystemVersion` from its `Info.plist`. If it changed, change `depends_on macos` in `scripts/ci/release/wisp.rb.template` and the minimum macOS in the README's Install section to match. Otherwise the release dry run's `min_os` audit fails.
6. **Review the diff.** In `git diff -- editor/patches`, changed line numbers and `index` lines are expected. Changed `+` or `-` lines are the conflicts you resolved, so check them again. The overlay never conflicts, so also read upstream's changes to the files it owns, in `editor/vscode/`: `git diff <old tag> <new tag> -- product.json resources/darwin/code.icns`. A new upstream key may need a value or a `null` in `editor/product.json`, such as a new telemetry or update endpoint.
7. **Commit and open the PR.** Commit `editor/upstream.json`, `editor/patches/`, and `.nvmrc` with a message such as `chore: upgrade Code - OSS to 1.140.0 (#<issue>)`. In the PR body, list the Electron and Node versions if they changed, and any patch that needed more than a mechanical rebase.

### How long it takes

Measured on an M3 Pro, in a rehearsal from 1.138.0 to 1.139.0 with three throwaway patches:

- Two patches rebased cleanly. The third conflicted, because upstream changed the same line and turned a boolean parameter into an object.
- `upgrade` took 1.4 seconds, `prepare` 0.4 seconds, and a rebuild about 3 minutes.

Expect about 30 minutes with a small series and no conflicts, mostly rebuild and smoke test. Add 10 to 30 minutes per conflicting patch. Update this section when real upgrades show otherwise.

## Troubleshooting

- **A patch in `editor/patches` does not apply**: someone changed the pin without `upgrade`, or edited a patch by hand. Restore the previous pin, then run `upgrade`.
- **`editor/vscode` has commits that are not in `editor/patches`**: run `export-patches` to keep them, or `prepare --force` to drop them.
- **Native module or `node-gyp` errors**: check the Node version first. Then delete `~/Library/Caches/node-gyp` and `editor/vscode/node_modules`, and run `npm ci` again.
- **Starting over**: delete `editor/vscode/` and run `prepare`. It fetches 62 MB and takes a few seconds, but the build steps start from scratch.
