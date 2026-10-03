# Parallax

`plxd`, an open-source Rust host daemon, and an Electron desktop app in `apps/desktop/` ([0022](docs/decisions/0022-desktop-app.md)), for macOS, Windows, and Linux ([0023](docs/decisions/0023-cross-platform.md)). The plan and open questions live in [docs/PLAN.md](docs/PLAN.md). Read it before planning any work. Records in [docs/decisions/](docs/decisions/) supersede the plan where they differ.

## Roles

The session driving this repo acts as senior project manager: it plans, writes issues, assigns work, merges reviewed PRs into `main`, and delegates implementation and PR review to subagents. Every subagent is a senior specialist and its instructions open with its role:

- Senior software engineer (Rust daemon, desktop app, build tooling)
- Senior QA engineer (test plans, test code, regression checks)
- Senior DevOps engineer (CI/CD, release, packaging)

A subagent owns exactly one issue and works only on that issue's branch.

## Branches

The naming convention has no exceptions, including for small fixes.

| Branch | Purpose | Branches from | Merges into |
| --- | --- | --- | --- |
| `main` | Integration branch. A nightly is a snapshot of it; a stable release is one nightly, promoted ([0051](docs/decisions/0051-promote-a-nightly.md)) | | |
| `feature/<ID>-<slug>` | New functionality | `main` | `main` |
| `bug/<ID>-<slug>` | Bug fixes | `main` | `main` |
| `chore/<ID>-<slug>` | Tooling, deps, CI | `main` | `main` |
| `docs/<ID>-<slug>` | Documentation | `main` | `main` |
| `hotfix/<ID>-<slug>` | A stable fix that cannot wait for the next nightly to be promoted | `main` | `main` |

- Every branch starts from an existing Linear issue and uses its ID. The prefix follows the issue's type label (Feature, Bug, Chore, Docs; Improvement uses `feature/`), whatever its title says. Examples: `feature/PLX-12-connect-app-to-plxd`, `docs/PLX-5-linear-work-record`. Never use Linear's suggested branch name.
- Slugs are lowercase, hyphenated, five words or fewer.
- Commits use Conventional Commits (`feat:`, `fix:`, `chore:`, `docs:`, `test:`, `refactor:`) and end with the Linear ID, e.g. `feat: show follow-up messages in a rebuilt transcript (PLX-92)`.
- Commits and PRs are authored as Ryan, with the git identity `Ryan Stoffel <stoffel.thomas.ryan@gmail.com>` and no other email.
- The agent that writes a commit credits itself with one `Co-authored-by` trailer at the end of the commit message, and the squash message keeps it. Claude: `Co-authored-by: Claude <noreply@anthropic.com>`. Cursor: `Co-authored-by: Cursor <cursoragent@cursor.com>`. Any other agent uses the co-author trailer its own tool writes. No other trailers or session links.
- `main` is protected. Changes land only through pull requests.

## Issues are the record

The Linear project [Parallax](https://linear.app/ryanstoffel/project/parallax-459c0ee45806) is the single place work is tracked ([0021](docs/decisions/0021-linear-work-record.md)); GitHub is for code, PRs, and CI only. Plans, reasoning, decisions, progress, blockers, and questions go in issue comments; nothing important lives only in a local file or chat.

When something comes up mid-work (a bug, a follow-up, a question, out-of-scope work, a flaky test), open a Linear issue for it right away, under its milestone's epic, with labels and a link back to where it came up. Keep the current PR on its own issue.

- Labels: a type (Feature, Bug, Chore, Docs, Improvement), an Area (app, daemon, ci, release), and Blocked when waiting. Priority is Linear's priority field.
- Milestones M0 to M7 are the Linear project's milestones ([0021](docs/decisions/0021-linear-work-record.md)). Each has an epic issue (label Epic), and its tasks are the epic's sub-issues.
- A task issue is small enough for one PR and contains: problem statement, acceptance criteria as a checklist, assigned role, dependencies.
- Working an issue posts three comments in order:
  1. **Plan**: the approach, before any code.
  2. **Progress**: findings, decisions, and dead ends as they happen.
  3. **Handoff**: what changed, how it was tested, what is left open.
- A decision that affects more than one issue gets a record at `docs/decisions/NNNN-title.md`, linked from the issue.
- When work depends on an unanswered question for Ryan, add the Blocked label to the waiting issue and comment the question there, mentioning Ryan, then move on to other work. Do the same for anything only Ryan can do, such as creating a secret. For everything else, choose a reasonable default and record it in the issue.

## Pull requests

- Target `main`.
- The body follows `.github/pull_request_template.md`: the problem, the fix, a `Linear: <issue URL>` line, and the acceptance criteria as a checklist. Linear's GitHub integration links the PR and marks the issue Done when it merges.
- A separate senior engineer subagent reviews every PR against the acceptance criteria and posts it as a comment review (`gh pr review --comment`; the shared account cannot approve its own PRs). The review opens with `Verdict: ready to merge` or `Verdict: changes needed`.
- The project manager merges a PR into `main` once the review says ready and CI is green. Feature, bug, chore, and docs PRs are squash-merged.
- Ryan promotes a nightly to stable by running the Release workflow with `channel=stable` ([0051](docs/decisions/0051-promote-a-nightly.md)). A hotfix merges to `main` like any other pull request; to publish it before the next snapshot is promoted, Ryan runs that workflow with `commit` set to the hotfix commit.

## CI/CD

The GitHub Actions workflow `ci.yml` runs on every PR, and only on PRs, since `main` takes changes only through PRs that passed it: lint, type-check, build, and test `plxd` on macOS, Linux, and Windows and check `plxd attach` over ssh; lint, type-check, test, build, and launch the app on the same three OSes; run the app's end-to-end tests on macOS, Linux, and Windows against the `plxd` the Rust job built for each; lint the workflow. The `ci` job needs every other job, and a red `ci` check blocks merge. A second workflow, `release.yml`, builds the installer and its update metadata for macOS arm64, Windows x64 and arm64, and Linux x64 and arm64 on their own runners (no tests or lint; the PR already passed them), signing and verifying the macOS app (Windows and Linux stay unsigned) and reusing a cached release `plxd` when no Rust changed. It does not run on push. A schedule checks the default branch every half hour and publishes a nightly prerelease, `v<YYMM.1DDHH.1MMSS>-nightly`, only when that branch has commits since the last nightly and the last nightly is at least six hours old. A manual run promotes the latest nightly's commit to a standard release of the same timestamp, marked Latest, or publishes a nightly immediately. `plan` creates the draft, each build attaches its installer and the updater's `.yml`, zip, and blockmap files as soon as it's done and publishes it, so the first one done makes it visible; then a `finish` job joins the Windows metadata and adds `SHA256SUMS`, and a `notarize` job notarizes the published dmg ([0051](docs/decisions/0051-promote-a-nightly.md), [0028](docs/decisions/0028-release-channels.md), [0029](docs/decisions/0029-app-packaging.md), [0030](docs/decisions/0030-release-versions.md)). A failed build leaves only its own OS out. `workflow_dispatch` with `dry-run` builds, signs, and notarizes on any branch and never publishes. It is the only workflow with `contents: write`.

## Order of work

1. CI/CD, as `chore/` issues, before any product code.
2. Milestones in order, M0 through M7. Within a milestone, run independent issues in parallel.
