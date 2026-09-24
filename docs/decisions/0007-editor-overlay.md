# 0007: wisp's overlay on the editor tree

- Status: accepted
- Date: 2026-09-24
- Issue: #9

## Context

[0002](0002-editor-fork-strategy.md) keeps wisp's editor changes as a patch series, which `scripts/editor/prepare` applies with `git am`. It also makes two exceptions to patches:

- `product.json` changes go through a merge step in `prepare`, because upstream edits the file every week and VSCodium had to regenerate its branding patch in 11 of 12 updates.
- Larger wisp-owned code should be files that `prepare` copies in, not patches that add files.

As #8 left the scripts, such an overlay had nowhere to go:

- **Left uncommitted in `editor/vscode`**: the next `prepare` stops at its clean-tree check.
- **Committed**: `export-patches` exports it as a patch, and `--check` fails in CI.
- **Existing trees**: the stamp that lets `prepare` skip work hashed only the pin and the patches, so a prepared tree would never pick up overlay changes.

## Decision

The overlay is one commit that `prepare` makes on the pinned upstream commit. The patches apply on top of it.

**Inputs**, committed in wisp:

- `editor/product.json`: a [JSON merge patch](https://www.rfc-editor.org/rfc/rfc7396) over upstream's `product.json`. Objects merge key by key, `null` deletes a key, and any other value replaces the old one. Key order is kept, so `git diff` in the tree shows only what wisp changed.
- `editor/overlay/<path>`: copied to `<path>` in the tree, as a new file or replacing upstream's, with its executable bit.
  - The overlay holds only regular files. It may not hold `product.json` or `.git`.
  - Finder's `.DS_Store` files are skipped.

**`prepare`**:

1. Fetches the pin, as before.
2. Builds the overlay commit (the *base*) with a temporary index and `git commit-tree`. The author and committer are `wisp prepare`, and the date is the upstream commit's committer date. The same pin and overlay therefore give the same commit on any machine. Without an overlay, the base is the pinned commit itself.
3. Checks out the base on the branch `wisp`, runs `git am` for the patches, and records `refs/wisp/base` and `refs/wisp/applied`.
4. Stamps the tree with a hash of the pin, the patches, the overlay (contents and modes), and `scripts/editor/*`. A tree prepared by older scripts, or before an overlay change, is prepared again.

**`export-patches`** exports `refs/wisp/base..HEAD`, so the overlay is never a patch, and a commit made on top of it is always exported. It refuses commits that change `product.json` or a file the overlay provides: make those changes in `editor/product.json` or `editor/overlay` instead. `--check` compares as before.

**The clean-tree check** is unchanged. The overlay is committed, so the tree is clean after `prepare`.

**`upgrade <tag>`** builds the base for `<tag>` and rebases the patches with `git rebase --onto <new base> <old base>`. The overlay is made again for the new release, never rebased, so upstream's `product.json` edits cannot conflict.

`scripts/editor/test` covers each of these against a local stand-in for upstream.

### Rejected

| Option | Problem |
| --- | --- |
| Patches only | A branding patch breaks whenever upstream edits `product.json`: 11 of VSCodium's 12 updates in 2026. |
| The overlay as the last commit, with `export-patches` exporting up to the commit before it (proposed in the #46 review) | A commit made on top of the overlay in `editor/vscode`, the normal workflow, falls outside the exported range and is silently dropped. Exporting it instead means skipping a commit in the middle of the branch, and its diff carries the overlay's context. `upgrade` would have to remove the overlay commit before rebasing and add it again afterwards. |
| An uncommitted overlay | `git commit -a` in the tree sweeps it into a patch, and the clean-tree check needs exceptions for its paths. |

## Consequences

- Upgrades cannot conflict on `product.json` or on overlaid files. They also cannot flag an upstream change to a key wisp overrides, so an upgrade should still read `git diff <old tag> <new tag> -- product.json` in the tree.
- A patch's context never includes overlay content, because `export-patches` refuses patches that touch overlaid files.
- Wisp-owned files are edited in wisp, then `prepare` runs again, which takes a second or two when only the overlay changed. `checkout` rewrites only the files that differ, so an incremental build or `npm run watch` keeps working.
- The packaged app reports the tree's HEAD as its commit, upstream's default, which answers the question 0002 left to #9. HEAD is the last patch commit, and `prepare` derives it deterministically from the pin, the overlay, and the patches. A cached build and a fresh build of the same inputs therefore report the same commit. It is not a commit in any public repository, and the release version (#43) is what identifies a wisp release.
