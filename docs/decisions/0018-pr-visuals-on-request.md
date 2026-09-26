# 0018: PR visuals are captured on request, into the PR body

- Status: accepted
- Date: 2026-09-26
- Issue: #244

## Context

`CLAUDE.md` said `screenshots.yml` runs on every PR and captures startup, the editor with a file open, and the coordinator chat, plus any view the PR touches, then posts them as one PR comment. In practice every PR got the same generic set, whether or not it changed anything a user sees, and the relevant capture, when there was one, was buried among the rest. Ryan never meant the rule that way. Visuals belong only where they show what changed:

- a new feature with UI: captures of the new view or flow
- a user-visible bug fix: a before and after pair
- behavior that only makes sense in motion: a short video
- backend, CI, docs, refactor, and test-only PRs: nothing

And they belong in the PR body, where a reviewer reads the change, not in a comment.

## Decision

This supersedes the "every PR" rule in `CLAUDE.md` and the comment it described.

- **Opt in with a label.** `screenshots.yml` does nothing unless the PR has the `screenshots` label. The job-level `if` stops an unlabeled PR before any runner starts, and the `request` job checks the label again against the API. It runs when the label is added, and on `synchronize`, `reopened`, and `edited` while the label is there. An `edited` event that changes neither the request block nor the base branch is skipped, so fixing a typo in the description does not capture again. `workflow_dispatch` with a `pr` input runs it by hand, still only for a labeled PR.
- **The body names the scenes.** A hidden block in the PR body lists what to capture, by the scene names in `ci/screenshots/src/scenarios.ts`:

  ```
  <!-- wisp-media
  after: agents-window-project
  before-after: agents-window-disconnected
  video: agents-window-subagent
  -->
  ```

  One `mode: scene, scene` line per mode. Blank lines and `#` comments are skipped, and `;` also ends a line. A scene appears at most once; `before-after` already includes the after. At most 10 scenes. There is no default set: a labeled PR with no block, a malformed line, or an unknown scene fails the `request` job with a message that says what to fix, and that message is written into the body's section too.
- **Three modes.**
  - `after` captures the scene from the head commit's app.
  - `before-after` captures it from the base app and from the head app, shown side by side in a table. The base is the merge base of the PR's base branch and its head, so the pair differs by this PR only. Its app is restored from `ci.yml`'s cache under the key that commit's own `app-cache-key` computes, and built on a miss. A scene that fails on the base app does not fail the job: for a bug fix, that failure is often the bug, and its screenshot is shown as the before.
  - `video` records the scene with Playwright's `recordVideo`, keeps the window's WebM, and makes a GIF preview with Homebrew's ffmpeg, installed only when a video is requested. The body embeds the GIF, which GitHub renders from a raw URL, and links the WebM.
- **The body holds the section.** `publish` writes a `## Screenshots` section between `<!-- wisp-media:start -->` and `<!-- wisp-media:end -->`, appending it the first time and replacing only that span afterward. It reads the body again immediately before the write, so an edit made during the capture is kept, and it never posts a comment. It writes nothing when the label is gone or the request block changed since the run read it, so an older capture cannot overwrite a newer section, and it refuses, with an error, a start marker with no end marker or a body that ends in an unclosed code fence, rather than drop or bury the author's text. Removing the label leaves the section as it is.
- **Storage is unchanged.** Files stay on the orphan branch `ci-screenshots` under `pr-<number>/<short-sha>/`, linked by raw URLs pinned to the `ci-screenshots` commit.
- **The security model holds.** The first step of the `request` job, trusted workflow code that runs before anything from the PR is checked out, stops a PR from a fork, an unlabeled PR, and a closed PR, for every event. That includes `workflow_dispatch`, whose token can write even when the PR number is a fork's, so a fork's code never reaches `publish`. The PR's own `resolve.ts` checks again, as a second line only. No job saves to Actions' cache after PR code ran in it, because a dispatched run would save into `develop`'s scope. The `request` and `capture` jobs have read-only tokens. `publish` alone can write, installs nothing, and treats both the artifact and the request job's outputs as untrusted: it re-checks the problem text, and nothing untrusted can write `<!--` into the section, so it cannot forge the end marker or a request block.
- **Not a required check.** `develop` requires only `ci`, so an unlabeled PR, whose `screenshots` jobs show as skipped, merges as before.

## Consequences

- Most PRs cost no macOS minutes for screenshots. A PR that asks for visuals pays for exactly those scenes, plus a second app build on a `before-after` whose base was evicted from the cache, and about a minute of Homebrew for a video.
- Whoever opens a UI PR decides what shows the change, and a scene that does not exist yet has to be added to `scenarios.ts` in the same PR. The PR template's Visuals section says when to add the label and how to write the block.
- The reviewer checks the visuals against the PR: a UI change with no section, or a section on a PR that changes nothing visible, is a finding.
- The old comments stay on existing PRs until #245 removes them and moves the useful captures into those PRs' bodies in this format.
