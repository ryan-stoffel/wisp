# 0002: The editor is upstream Code - OSS at a pinned tag plus a patch series

- Status: accepted
- Date: 2026-09-23
- Issue: #8

## Context

wisp's editor is a stripped-down fork of Code - OSS ([PLAN](../PLAN.md)). The plan names fork upkeep as a top risk. The way the fork is vendored decides that cost for the life of the project.

Upstream facts, as of 2026-09-23:

- Code - OSS has shipped a stable release every week since March 2026: 1.110.0 on 2026-03-04, 1.139.0 on 2026-09-23.
- Upstream moves fast. From 1.138.0 to 1.139.0, a single week, 1,478 files changed (+126,090 / -19,285 lines). Over four releases, 1.135.0 to 1.139.0, 4,539 files changed. Most of the churn is in chat, agent sessions, the agent host, and the built-in Copilot extension.
- `microsoft/vscode` takes about 1.5 GB on GitHub. A depth-1 fetch of 1.139.0 is 62 MB, and the checked-out tree takes 372 MB in 19,055 files.

wisp will change the editor in #9 (branding, Open VSX, telemetry), #10 (stripping the workbench), and #12 (the coordinator chat view), and later for the daemon integration.

### How other forks do it

| Approach | Project | How upstream arrives | Observed upkeep |
| --- | --- | --- | --- |
| Full fork, merged | [Positron](https://github.com/posit-dev/positron) | Upstream changes merged on `merge/...` branches, squash-merged | 11 upstream merges since July 2025, each landing 12 to 54 days after the upstream release. Merge PRs took 8 and 11 days ([#15768](https://github.com/posit-dev/positron/pull/15768), [#15243](https://github.com/posit-dev/positron/pull/15243)). Now on 1.134.0, five releases behind. It keeps `.removed-upstream-dependencies` because merges bring back dependencies it deleted. |
| Full fork, rebased | [openvscode-server](https://github.com/gitpod-io/openvscode-server) | 17 commits rebased onto each upstream minor (`scripts/sync-with-upstream.sh`) | Last release 1.109.5 on 2026-02-20 |
| Subtree | [che-code](https://github.com/che-incubator/che-code) | `git subtree pull` into `code/` with upstream history, then 101 scripted rebase rules | The 1.116 rebase ran from April 22 to June 16, and the 1.128 rebase from July 14 to August 5. One pull changed 6,094 upstream files. |
| Submodule plus patches | [code-server](https://github.com/coder/code-server) | `lib/vscode` submodule plus a 27-patch quilt series, refreshed by a bot PR twice a day | Each minor from 1.127 to 1.138 merged 2 to 7 days after upstream. Its contributing guide says it went from a submodule with one large patch, to a subtree, to the current setup. |
| Pinned tag plus patches | [VSCodium](https://github.com/VSCodium/vscodium) | `upstream/stable.json` pins tag and commit; `get_repo.sh` fetches that commit with depth 1; `prepare_vscode.sh` applies 70 patches with `git apply` and edits `product.json` with `jq` | Ships every 4th to 9th weekly release, 2 to 15 days after upstream. In 2026, 11 of its 12 "update patches" commits regenerated the same branding, keymap, and Copilot patches. The keymap and policy patches touch `package.json` and `package-lock.json`. It turned off automatic release builds because "they always need to be manually validated". |
| Submodule plus `git am` | [GitLab Web IDE fork](https://gitlab.com/gitlab-org/gitlab-web-ide-vscode-fork) | Replaced a full fork with a submodule and a `git format-patch` series ([!87](https://gitlab.com/gitlab-org/gitlab-web-ide-vscode-fork/-/merge_requests/87)) | |

The sources point the same way. Small patch stacks land within days of upstream, and forks that carry upstream's history lag by weeks. Patches to `package.json`, lockfiles, and branding break on almost every release. Positron wraps every edit to an upstream file in `// --- Start Positron ---` markers so that merges can find them.

## Options

1. **Full fork.** Upstream's tree and history live in wisp, and upgrades are merges or rebases.
   - Everyday development is plain: every file is in the repo.
   - wisp's history gains about 1.5 GB.
   - Each upgrade brings in thousands of upstream file changes, so an upgrade PR cannot really be reviewed.
   - wisp's own changes can be found only by diffing against upstream or through code markers.
   - Positron, che-code, and openvscode-server show the lag this produces.
2. **Submodule plus patches.** A gitlink pins upstream, and the patches are still needed.
   - Compared with option 3, it only changes how the pin is stored: a SHA in the gitlink instead of a line in a JSON file. That is harder to review.
   - It adds a submodule checkout step and a detached checkout that is easy to commit to by accident.
3. **Pinned tag plus a patch series.** wisp commits a pin and its patches, and a script produces the tree.
   - The repo stays small.
   - The series lists every way wisp differs from upstream.
   - An upgrade PR shows a one-line pin change and the patch refreshes.
   - Editing a patch takes an export step.
   - Changes to a patch are reviewed as diffs of diffs.
   - Code navigation needs a prepared tree.
4. **Subtree.** Like a full fork, with `git subtree` tooling on top. che-code's rebases took weeks.

## Decision

Option 3: upstream Code - OSS at a pinned tag, plus a patch series applied by one script.

- `editor/upstream.json` pins the repository, tag, and commit. The first pin is 1.139.0 (`2242ebbb54efeeb0129e08e919e7e8d43033cd83`).
- `editor/patches/NNNN-*.patch` holds wisp's changes in `git format-patch` format, applied in filename order. Each file has a subject and a description of why the change exists. The series starts empty. The first patches belong to #9 and #10.
- `scripts/editor/prepare` does the following. The tree it produces is ready for upstream's own build (`npm ci`, `npm run electron`, `npm run compile`).
  - It fetches the pinned tag with depth 1 into `editor/vscode/`, and fails if the tag no longer points at the pinned commit.
  - It applies the series with `git am`, one commit per patch on a local `wisp` branch.
  - The committer and dates are fixed, and global git config is ignored, so the same pin and patches produce the same commits on any machine.
  - It refuses to overwrite uncommitted or unexported work in the tree.
- Patches are commits, not plain `git apply` diffs as in VSCodium. That lets an upgrade use `git rebase`, with three-way merges and normal conflict tools, instead of `.rej` files. `scripts/editor/export-patches` writes the commits back in a canonical format, and `scripts/editor/upgrade <tag>` does the rebase, the pin, and the export. The GitLab Web IDE fork uses the same `format-patch` and `git am` pair.
- The root `.nvmrc` equals upstream's `.nvmrc` (Node 24.18.0 for 1.139.0), because the CI scripts check Node against the root file. `upgrade` copies it, and `scripts/ci/check-fork` fails if the two differ.
- `scripts/ci/check-fork` runs in CI's `fork` job on Ubuntu:
  - It tests the editor scripts.
  - It runs `prepare` and `export-patches --check`.
  - It runs upstream's TypeScript type-check (`npm run typecheck-client`), which covers `src/`.

  A patch that no longer applies, was edited by hand, or breaks the types in `src/` therefore fails CI. A PR skips the check when the same inputs already passed it. That skip is advisory: pushes to `develop` always run the check, and that run is the check of record (`scripts/ci/README.md`). The full build and its caching are left to #9.

### What is committed

| Path | In git | Why |
| --- | --- | --- |
| `editor/upstream.json` | Yes | The pin. An upgrade changes two lines: the tag and the commit. |
| `editor/patches/` | Yes | wisp's changes, reviewable as text |
| `scripts/editor/` | Yes | The tooling |
| `editor/vscode/` | No, gitignored | Rebuilt from the two above with one command. It takes 372 MB after `prepare` and 7.5 GB after a build, with `node_modules`, `out`, and `.build`. Committing the source would add upstream's churn (1,478 files in one week) to every upgrade PR. |

### Rules for patches

These keep the series cheap to carry. They come from what broke for the forks above.

1. Prefer, in order: configuration (`product.json`, default settings), new files or a wisp-owned built-in extension, and only then small edits to upstream files.
2. Change `product.json` with a merge step in `prepare`, not a patch. Upstream edits it often, and VSCodium's branding patch had to be regenerated in 11 of 12 updates. [0008](0008-editor-overlay.md) describes the merge step, `editor/product.json`.
3. Do not patch `package.json` or lockfiles unless there is no other way.
4. Strip features by excluding them from the build or from registration, not by deleting upstream files. A deletion patch carries every deleted line and conflicts with every upstream edit to those files.
5. Keep wisp's features out of upstream's fastest-moving code: chat, sessions, the agent host, and Copilot. The coordinator chat (#12) should be its own contribution, not an edit to upstream's chat.
6. Give each patch one concern, a subject that starts with its area (`branding:`, `strip:`, `chat:`), and a description of why it exists. The patch files themselves mark what wisp changed, so upstream files need no Positron-style markers.

### Upgrades

The step-by-step procedure is in [editor-upgrade.md](../editor-upgrade.md):

1. Run `scripts/editor/prepare`.
2. Run `scripts/editor/upgrade <tag>`. If it stops on a conflict, resolve it with git, continue the rebase, and run it again.
3. Run `prepare` again.
4. Rebuild and smoke-test.
5. Commit the pin, the patches, and `.nvmrc`.

Cadence: move to the newest stable release about every four weeks, and within a week when a release fixes an Electron or Chromium security issue. Weekly upgrades would cost more than they return. Only files that wisp patches can conflict, and resolving four weeks of upstream changes to a file at once is no harder than resolving them a week at a time.

Time, measured on an M3 Pro with a rehearsal from 1.138.0 to 1.139.0 with three throwaway patches:

- Two patches rebased cleanly. The third conflicted, because upstream changed the same line and turned a boolean parameter into an object.
- `upgrade` took 1.4 seconds and `prepare` 0.4 seconds.
- Resolving the conflict meant reading the upstream change: minutes for a person.
- A rebuild takes about 3 minutes: `npm ci` 2 min 20 s cold (2 min warm), `npm run electron` 10 s, `npm run compile` 27 s.

Expect about 30 minutes per upgrade with a small series and no conflicts, mostly rebuild and smoke test. Add 10 to 30 minutes for each patch that conflicts. Budget one to two hours per monthly upgrade until the Playwright smoke tests (#13) run in CI, and revisit the estimate after the first few upgrades.

## Consequences

- Upgrades are reviewable: a pin change plus patch refreshes. Hunk changes in the patches show exactly where a conflict was resolved.
- The series is the complete list of how wisp differs from upstream, and CI proves on every PR that it still applies.
- wisp's repo stays small. `check-fork` fetches 62 MB of upstream when it runs, which takes seconds.
- Editing the editor takes a round trip: `prepare`, then commit in `editor/vscode`, then `export-patches`, then commit the patch. Code navigation and IDE features need a prepared tree.
- Changes to a patch are reviewed as diffs of diffs. If #12 or later work adds a lot of new code, move it into a wisp-owned directory or built-in extension that `prepare` copies in, instead of growing patches that add files. `editor/overlay/` is that directory ([0008](0008-editor-overlay.md)).
- When patches exist, the tree's HEAD is a local commit, not an upstream one. It is reproducible from the pin, the overlay, and the series. A packaged build reports it as its commit ([0008](0008-editor-overlay.md)).
- The built-in extension download (`npm run download-builtin-extensions`) calls the GitHub REST API, which counts against the caller's budget. CI should pass its own `GITHUB_TOKEN`.
