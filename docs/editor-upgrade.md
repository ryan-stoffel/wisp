# Working on the editor and upgrading Code - OSS

wisp's editor is upstream Code - OSS at the release pinned in `editor/upstream.json`, plus the patches in `editor/patches/`. [0002](decisions/0002-editor-fork-strategy.md) explains why. This page covers building the editor, changing wisp's patches, and moving to a newer upstream release.

## Layout

| Path | What it is | In git |
| --- | --- | --- |
| `editor/upstream.json` | The pin: upstream repository, release tag, and the commit the tag must resolve to | Yes |
| `editor/patches/NNNN-*.patch` | wisp's changes, one `git format-patch` file per commit, applied in filename order | Yes |
| `editor/vscode/` | Upstream at the pin with the patches applied as commits on the branch `wisp`. It also holds `node_modules`, `out`, and `.build` after a build. | No |
| `scripts/editor/prepare` | Creates or updates `editor/vscode/` from the pin and the patches | Yes |
| `scripts/editor/export-patches` | Writes the commits in `editor/vscode/` back to `editor/patches/` | Yes |
| `scripts/editor/upgrade <tag>` | Rebases the patches onto another upstream release and updates the pin | Yes |
| `scripts/editor/test` | Tests the three scripts above against a local stand-in for upstream, without network access. Run it after changing them. CI runs it in `check-fork`. | Yes |

`prepare` never overwrites work. It stops if `editor/vscode/` has uncommitted changes, untracked files, or commits that were not exported. `--force` discards them.

## Prerequisites

- macOS with Xcode and its command line tools. Native modules build with clang.
- Python 3, for `node-gyp`.
- git 2.34 or later.
- Node as pinned in the root `.nvmrc`, which always equals upstream's `editor/vscode/.nvmrc` (24.18.0 for 1.139.0). Upstream's `npm ci` refuses an older Node or another major version. With fnm or nvm, run `fnm use` or `nvm use` in the repo root.
- About 10 GB of free disk: 7.5 GB for `editor/vscode/` after a build, plus about 1.6 GB in the npm, node-gyp, Electron, and Playwright caches.

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

The times are from an M3 Pro. `scripts/ci/build-app` with `WISP_APP=editor` runs the same steps. `WISP_APP=editor scripts/ci/app-launch` prints the Playwright launch options for the result (see [scripts/ci/README.md](../scripts/ci/README.md)).

`npm ci` also downloads Playwright's Chromium (550 MB) for upstream's browser tests. Set `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` to skip it when you do not run those tests. `build-app` sets it.

## Change a patch or add one

1. Run `scripts/editor/prepare`.
2. Edit, build, and test in `editor/vscode/`.
3. Commit there.
   - For a new patch, make a new commit. The subject becomes the file name, so start it with the area (`branding:`, `strip:`, `chat:`). The body says why wisp needs the change.
   - To change an existing patch, commit with `git commit --fixup=<commit>`, then fold it in with `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash <pinned commit>`. `git log` in `editor/vscode/` shows which commit is which patch.
4. Run `scripts/editor/export-patches`. It rewrites `editor/patches/` from the commits.
5. Commit `editor/patches/` in wisp. CI runs `export-patches --check`, which fails if the committed files differ from what an export would write, for example after a hand edit. It also type-checks `src/` in the patched tree.

Keep patches cheap to carry across upgrades. [0002](decisions/0002-editor-fork-strategy.md#rules-for-patches) has the rules. In short:

- Prefer configuration or new files over edits to upstream files.
- Do not patch lockfiles.
- Strip features by excluding them, not by deleting files.
- Stay out of upstream's chat and agent code.

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

   The script fetches the tag and rebases the commits in `editor/vscode/` onto it. Then it rewrites `editor/upstream.json`, copies upstream's `.nvmrc` to the root `.nvmrc`, and exports the patches.

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

5. **Rebuild and smoke-test.** Run `fnm use` or `nvm use` in the repo root first, because `upgrade` may have changed `.nvmrc`, and upstream's `npm ci` rejects an older Node. Then run the build steps above and launch the development build. Open a folder, open a file from the tree, edit and save it, and open a terminal. `npm ci` is needed because upstream's lockfile changes with almost every release. Once #13 lands, run its Playwright smoke tests too.
6. **Review the diff.** In `git diff -- editor/patches`, changed line numbers and `index` lines are expected. Changed `+` or `-` lines are the conflicts you resolved, so check them again.
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
