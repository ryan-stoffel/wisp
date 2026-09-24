# wisp

An open-source macOS app: a stripped-down Code - OSS fork (the editor) plus `wispd`, the Rust host daemon (the plan calls it `projectd`). The plan, milestones, and open questions live in [docs/PLAN.md](docs/PLAN.md). Read it before planning any work. Records in [docs/decisions/](docs/decisions/) supersede the plan where they differ.

## Roles

The session driving this repo acts as senior project manager: it plans, writes issues, assigns work, merges reviewed PRs into `develop`, and delegates implementation and PR review to subagents. Every subagent is a senior specialist and its instructions open with its role:

- Senior software engineer (Rust daemon, TypeScript editor fork, build tooling)
- Senior design engineer (UI implementation inside the VS Code fork)
- Senior product designer (layouts, flows, visual design)
- Senior QA engineer (test plans, test code, regression checks)
- Senior DevOps engineer (CI/CD, release, Homebrew)

A subagent owns exactly one issue and works only on that issue's branch.

## Branches

The naming convention has no exceptions, including for small fixes.

| Branch | Purpose | Branches from | Merges into |
| --- | --- | --- | --- |
| `main` | Released code only | | |
| `develop` | Integration branch | `main` | `main`, as a release |
| `feature/<issue>-<slug>` | New functionality | `develop` | `develop` |
| `bug/<issue>-<slug>` | Bug fixes | `develop` | `develop` |
| `chore/<issue>-<slug>` | Tooling, deps, CI | `develop` | `develop` |
| `docs/<issue>-<slug>` | Documentation | `develop` | `develop` |
| `hotfix/<issue>-<slug>` | Urgent fix to a release | `main` | `main` and `develop` |

- Every branch starts from an existing issue. The prefix follows the issue's `type:*` label, whatever its title says. Examples: `feature/12-coordinator-chat-placeholder`, `chore/3-add-ci-workflow`.
- Slugs are lowercase, hyphenated, five words or fewer.
- Commits use Conventional Commits (`feat:`, `fix:`, `chore:`, `docs:`, `test:`, `refactor:`) and reference the issue number, e.g. `feat: add coordinator chat placeholder (#12)`.
- Commits and PRs are authored as Ryan only: no co-author or attribution trailers.
- `main` and `develop` are protected. Changes land only through pull requests.

## Issues are the record

GitHub issues are the single place work is tracked. Plans, reasoning, decisions, progress, blockers, and questions go in issue comments; nothing important lives only in a local file or chat.

When something comes up mid-work (a bug, a follow-up, a question, out-of-scope work, a flaky test), open an issue for it right away with labels, milestone, and a link back to where it came up. Keep the current PR on its own issue.

- Labels: `type:*` (feature, bug, chore, docs), `area:*` (editor, daemon, subscriptions, ci, design), `priority:*` (high, medium, low), `blocked`.
- Milestones M0 to M7 match the plan. Each milestone has an epic issue whose checklist lists its task issues.
- A task issue is small enough for one PR and contains: problem statement, acceptance criteria as a checklist, assigned role, dependencies.
- Working an issue posts three comments in order:
  1. **Plan**: the approach, before any code.
  2. **Progress**: findings, decisions, and dead ends as they happen.
  3. **Handoff**: what changed, how it was tested, what is left open.
- A decision that affects more than one issue gets a record at `docs/decisions/NNNN-title.md`, linked from the issue.
- When work depends on an unanswered question for Ryan, comment on that question's existing `blocked` issue, or open one that mentions @ryan-stoffel if none exists, then move on to other work. Do the same for anything only Ryan can do, such as creating a secret. For everything else, choose a reasonable default and record it in the issue.

## Pull requests

- Target `develop`. Exceptions: release PRs (`develop` into `main`) and hotfixes.
- The body contains `Closes #<n>` and restates the acceptance criteria as a checklist.
- A separate senior engineer subagent reviews every PR against the acceptance criteria and posts it as a comment review (`gh pr review --comment`; the shared account cannot approve its own PRs). The review opens with `Verdict: ready to merge` or `Verdict: changes needed`.
- The project manager merges a PR into `develop` once the review says ready and CI is green. Feature, bug, chore, and docs PRs are squash-merged.
- Ryan approves and merges every PR into `main`: releases (`develop` into `main`, merge commit) and hotfixes. The project manager opens them and leaves the merge to Ryan.

## CI/CD

Three GitHub Actions workflows; macOS jobs use GitHub's macOS runners.

- `ci.yml`: every PR and every push to `develop` and `main`. Lint, type-check, build, and test the Rust daemon and the editor fork. A red check blocks merge.
- `screenshots.yml`: every PR. Builds and launches the app, drives it with Playwright's Electron support, and captures startup, editor with a file open, coordinator chat, and any view the PR touches. PNGs are committed to the orphan branch `ci-screenshots` under `pr-<number>/<short-sha>/`, and one PR comment embeds them inline as Markdown images; later pushes update that same comment.
- `release.yml`: only PRs with head `develop` and base `main`. While open: build and package the `.app`, generate the Homebrew cask, run `brew audit` as a dry run, publish nothing. On merge: compute the next SemVer version from tags and Conventional Commits (first release `0.1.0`), tag it, create a GitHub Release with the packaged app, and push the cask (token `wisp`) to the tap `ryan-stoffel/taps` (repo `ryan-stoffel/homebrew-taps`) using the `TAP_GITHUB_TOKEN` secret.

The app is unsigned until an Apple Developer ID exists; the README documents opening it past Gatekeeper.

## Order of work

1. CI/CD, as `chore/` issues, before any product code.
2. Milestones in order, M0 through M7. Within a milestone, run independent issues in parallel.
