# 0005: Shared context is a daemon-owned folder outside the repo

- Status: accepted
- Date: 2026-09-23
- Issue: #16

## Context

Shared context is a folder of Markdown files that every agent reads and writes: research, test instructions, and the user's preferences. The plan asked whether it syncs as git commits in the project repo or as a separate folder that the daemon copies. Ryan chose the separate folder: `wispd` owns it on the host, copies it to other machines, and never commits it to git (#16). Everything else under Decision is a default chosen in #30, not Ryan's call.

## Decision

- Each project's shared context lives in a folder owned by `wispd` on the host, outside the project's git repo, under wisp's application data directory.
- The copy on the host is the source of truth. Other machines, such as the MacBook running a local agent (M5), get a mirror that `wispd` keeps in sync over its existing connection to that machine.
- Agents find the folder through a path `wispd` gives them when they start: in their prompt and, for vendor CLIs, as an extra allowed directory. They never search for it in the worktree.
- Two machines can change the same file before a sync. When that happens, the host's version wins, and the other version is kept next to it as a conflict copy, so no agent's notes are lost across machines. On the host itself, agents share one folder and simultaneous writes to the same file are last-writer-wins, so agents should add new files rather than rewrite shared ones.

## Consequences

- Agent notes stay out of the user's git history and out of PR diffs.
- The folder is not versioned by git, so `wispd` is responsible for backups and history, if we want them.
- M3 builds the host side (read and write from a worktree). M5 builds the mirror and the sync back to the host.
