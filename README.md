<p align="center">
  <img src="design/icon/masters/Parallax-dark-legacy-1024.png" alt="Parallax" width="160">
</p>

<h1 align="center">Parallax</h1>

<p align="center">An open-source desktop app that runs coding agents on computers you own.</p>

<p align="center">
  <img src="docs/images/agent.gif" alt="An agent working in Parallax: it starts a thread, reads the README, edits it, and replies." width="720">
</p>

Parallax is the Cursor Projects workflow on a machine you control. A project is one coordinator chat: it plans the work and hands pieces to subagents, each in its own git worktree, with a shared folder of project context. A thread is a single agent chat, in a repository or in none.

The window is an Electron app for macOS, Windows, and Linux. The work runs in [`plxd`](daemon/README.md), a Rust daemon on the host: this computer, or another one you reach over SSH. A project on a remote host keeps running with the laptop closed. You sign in to each vendor's own CLI on the host, and Parallax runs that CLI. Claude Code, Codex, and Cursor are the ones it runs today, with API keys as a fallback.

<p align="center">
  <img src="docs/images/new-thread.png" alt="The Parallax window, ready for a new thread in a repository named website." width="720">
</p>

<p align="center">
  <img src="docs/images/thread.png" alt="A thread after the agent read the README, edited it, and finished." width="720">
</p>

The sidebar lists projects and threads. The main pane is the chat. A thread starts from the composer: pick a repository, say what you want, and the agent works in a worktree on the host. You review the result from the same window.

The full plan is in [docs/PLAN.md](docs/PLAN.md).

## Run it

`plxd` builds with Cargo. The app needs Node 24 and pnpm through corepack (`corepack enable`). Run pnpm inside `apps/desktop`, where corepack finds the pinned version.

```sh
cargo build --release -p plxd

cd apps/desktop
pnpm install
pnpm dev
```

`pnpm check` formats, lints, and type-checks the app. `pnpm test` and `pnpm build` run the rest. `scripts/ci/check-rust` runs the same lint, build, and tests as CI for `plxd`.

## License

[Apache-2.0](LICENSE)
